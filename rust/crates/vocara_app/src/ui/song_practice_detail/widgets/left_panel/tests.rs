//! The inspector's tests: `left_panel_test.dart` — the channel-strip
//! footer's aux sends, output routing, pan, fader and mute/solo for a
//! selected take AND for a selected bus, through the same widgets but
//! different view-model methods; the short-inspector fallback; the metrics
//! heal; fresh state for another channel — plus the voice summary and the
//! pure parts.
//!
//! Two things are worth pinning here. First the DISPATCH: a bus and a take
//! share this UI and must never reach each other's methods. Second the send
//! menus' OFFER SETS: "add" must not offer a bus a slot already feeds, and
//! "retarget" must offer the slot's own current target plus the unused ones.
//! Two sends into one bus feed it twice, so the bus hears the sum and
//! neither fader reads what you hear — the view model refuses that, and
//! this is the half of the guard the user actually sees.
//!
//! The panel is driven on a [`LiveUi`], which keeps widget state from one
//! view to the next as the app does: the screen opens a menu by rebuilding
//! the panel with it open.

use std::f64::consts::PI;

use iced::{Settings, Size};
use iced_test::selector::Candidate;
use vocara_model::models::audio_fx::TrackFx;
use vocara_model::models::audio_track::AudioTrackKind;
use vocara_model::util::dart_num::pow_f64;

use super::*;
use crate::ui::core::theme;
use crate::ui::core::widgets::studio_controls::studio_fader;
use crate::ui::song_practice_detail::widgets::live_ui::{move_to, press, release, LiveUi};

/// The inspector pins its footer only above a minimum height: a real
/// window, so these tests exercise the pinned layout the app ships.
const WINDOW: Size = Size {
    width: 1440.0,
    height: 1200.0,
};

/// The strip's content width: the panel less its 1 px right border.
const CONTENT_W: f32 = PANEL_WIDTH - 1.0;

fn id(m: Message) -> Message {
    m
}

fn vocal(id: &str) -> AudioTrack {
    AudioTrack::new(
        id,
        AudioTrackKind::Vocal,
        "Take 1",
        DateTime::utc_ymd(2026, 1, 1),
    )
    .with_audio("/x.wav", 0, 5000, Vec::new())
}

fn with_sends(mut t: AudioTrack, buses: &[&str]) -> AudioTrack {
    t.sends = buses.iter().map(|b| TrackSend::new(*b, 0.0)).collect();
    t
}

fn bus(id: &str, name: &str) -> Arc<FxBus> {
    Arc::new(FxBus::new(id, name, TrackFx::flat()))
}

/// What the panel is pumped with (`_FakeVm`).
struct Vm {
    track: Option<AudioTrack>,
    bus: Option<Arc<FxBus>>,
    buses: Vec<Arc<FxBus>>,
    open_menu: Option<InspectorMenu>,
    /// The panel's height: the window's when `None`.
    height: Option<f32>,
}

impl Vm {
    fn new() -> Self {
        Vm {
            track: None,
            bus: None,
            buses: Vec::new(),
            open_menu: None,
            height: None,
        }
    }

    fn track(mut self, t: AudioTrack) -> Self {
        self.track = Some(t);
        self
    }

    fn bus(mut self, b: Arc<FxBus>) -> Self {
        self.bus = Some(b);
        self
    }

    fn buses(mut self, buses: Vec<Arc<FxBus>>) -> Self {
        self.buses = buses;
        self
    }

    fn height(mut self, h: f32) -> Self {
        self.height = Some(h);
        self
    }

    fn inspector(&self) -> Inspector<'_> {
        Inspector {
            open: true,
            selected_track: self.track.as_ref(),
            selected_bus: self.bus.as_deref(),
            fx_buses: &self.buses,
            supports_effects: true,
            active_interpretation: Some("Interpretation 1"),
            open_menu: self.open_menu.as_ref(),
        }
    }

    /// The panel at the window's top-left, with an empty FX slot.
    fn view(&self) -> Element<'_, Message> {
        self.view_with(|| Space::new().width(Length::Fill).height(Length::Fill).into())
    }

    fn view_with<'a>(&'a self, fx: impl Fn() -> Element<'a, Message> + 'a) -> Element<'a, Message> {
        let panel = left_panel(self.inspector(), fx, id);
        container(panel)
            .height(self.height.map_or(Length::Fill, Length::Fixed))
            .into()
    }
}

/// Every text on screen: its content, bounds and visible bounds.
fn texts(
    ui: &mut LiveUi,
    view: Element<'_, Message>,
) -> Vec<(String, Rectangle, Option<Rectangle>)> {
    let mut found = Vec::new();
    let _ = ui.find(view, |candidate: Candidate<'_>| {
        if let Candidate::Text {
            content,
            bounds,
            visible_bounds,
            ..
        } = candidate
        {
            found.push((content.to_string(), bounds, visible_bounds));
        }
        None::<()>
    });
    found
}

fn count(ui: &mut LiveUi, vm: &Vm, label: &str) -> usize {
    texts(ui, vm.view())
        .iter()
        .filter(|(c, _, _)| c == label)
        .count()
}

/// The bounds of the `nth` text reading `label`.
fn nth(ui: &mut LiveUi, vm: &Vm, label: &str, nth: usize) -> Rectangle {
    texts(ui, vm.view())
        .into_iter()
        .filter(|(c, _, _)| c == label)
        .nth(nth)
        .unwrap_or_else(|| panic!("{label} #{nth} is not on screen"))
        .1
}

fn bounds(ui: &mut LiveUi, vm: &Vm, label: &str) -> Rectangle {
    nth(ui, vm, label, 0)
}

/// A click on the `nth` text reading `label`.
fn tap_nth(ui: &mut LiveUi, vm: &Vm, label: &str, n: usize) -> Vec<Message> {
    let at = nth(ui, vm, label, n).center();
    ui.point_at(at);
    ui.run(vm.view(), iced_test::simulator::click())
}

fn tap(ui: &mut LiveUi, vm: &Vm, label: &str) -> Vec<Message> {
    tap_nth(ui, vm, label, 0)
}

fn ui() -> LiveUi {
    LiveUi::new(WINDOW)
}

/// Whether the strip is the scrolling column.
fn scrolls(ui: &mut LiveUi, vm: &Vm) -> bool {
    let mut found = false;
    let _ = ui.find(vm.view(), |candidate: Candidate<'_>| {
        if let Candidate::Scrollable { .. } = candidate {
            found = true;
        }
        None::<()>
    });
    found
}

// ------------------------------------------------------- left_panel_test.dart

#[test]
fn nothing_selected_shows_the_empty_state() {
    let vm = Vm::new();
    assert_eq!(
        count(&mut ui(), &vm, "Select a take or bus to inspect it."),
        1
    );
}

// A short window with the dock open (or dragged tall) leaves the inspector a
// sliver of height. The take strip scrolls there; the bus strip kept its
// pinned layout, overflowed, and pushed its fader, mute/solo and pan under
// the dock where nothing could reach them.
fn scroll_to_m_and_tap(vm: &Vm) -> Vec<Message> {
    let mut ui = ui();
    assert!(scrolls(&mut ui, vm), "a short strip scrolls");
    ui.point_at(Point::new(100.0, 100.0));
    let scroll = Event::Mouse(mouse::Event::WheelScrolled {
        delta: mouse::ScrollDelta::Lines { x: 0.0, y: -100.0 },
    });
    assert!(ui.run(vm.view(), [scroll]).is_empty());
    let (_, _, visible) = texts(&mut ui, vm.view())
        .into_iter()
        .find(|(c, _, _)| c == "M")
        .expect("M is on the strip");
    let visible = visible.expect("scrolled into view");
    ui.point_at(visible.center());
    ui.run(vm.view(), iced_test::simulator::click())
}

#[test]
fn a_short_bus_strip_scrolls_instead_of_overflowing() {
    let buses = vec![bus("a", "Reverb"), bus("b", "Delay")];
    let vm = Vm::new().bus(buses[0].clone()).buses(buses).height(200.0);
    assert_eq!(
        scroll_to_m_and_tap(&vm),
        vec![Message::ToggleBusMute("a".into())]
    );
}

#[test]
fn a_short_take_strip_scrolls_too() {
    let vm = Vm::new()
        .track(vocal("t1"))
        .buses(vec![bus("a", "Reverb")])
        .height(200.0);
    assert_eq!(
        scroll_to_m_and_tap(&vm),
        vec![Message::ToggleMute("t1".into())]
    );
}

#[test]
fn a_tall_strip_pins_its_footer() {
    let vm = Vm::new().track(vocal("t1")).buses(vec![bus("a", "Reverb")]);
    let mut ui = ui();
    assert!(!scrolls(&mut ui, &vm));
    // The footer sits at the bottom of the window-tall panel.
    let m = bounds(&mut ui, &vm, "M");
    assert!(m.y > WINDOW.height - 120.0, "{m:?}");
}

#[test]
fn the_inspector_asks_again_for_the_metrics_it_shows_and_only_those() {
    // Asking for the dock's halves, or for the pair, computes what the
    // inspector does not show.
    let vm = Vm::new().track(vocal("t1"));
    assert_eq!(
        vm.inspector().analysis_heal(true),
        Some(Message::HealSelectedAnalysis {
            spectrogram: Some(false),
            metrics: Some(true),
        })
    );
    assert_eq!(vm.inspector().analysis_heal(false), None);
    // Closed, or nothing to analyze: nothing.
    let closed = Inspector {
        open: false,
        ..vm.inspector()
    };
    assert_eq!(closed.analysis_heal(true), None);
    assert_eq!(Vm::new().inspector().analysis_heal(true), None);
    let mut backing = vocal("b");
    backing.kind = AudioTrackKind::Accompaniment;
    assert_eq!(
        Vm::new().track(backing).inspector().analysis_heal(true),
        None
    );
}

/// Where the volume slider is: left of its dB readout's column.
fn volume_fader(ui: &mut LiveUi, vm: &Vm) -> Point {
    let readout = bounds(ui, vm, "0.0 dB");
    let box_left = readout.x + readout.width - FOOTER_DB_WIDTH;
    Point::new(box_left - 10.0 - 30.0, readout.center_y())
}

fn volumes(messages: &[Message]) -> Vec<(String, f64)> {
    messages
        .iter()
        .filter_map(|m| match m {
            Message::SetTrackVolumeLive { track_id, volume } => Some((track_id.clone(), *volume)),
            _ => None,
        })
        .collect()
}

#[test]
fn another_take_selected_under_a_held_fader_gets_its_own() {
    // An import landing selects its take. Unkeyed, the inspector handed the
    // new take the fader the user was still holding, and the rest of the
    // drag set its volume.
    let t1 = Vm::new().track(vocal("t1"));
    let t2 = Vm::new().track(vocal("t2"));
    let mut ui = ui();
    let at = volume_fader(&mut ui, &t1);
    ui.point_at(at);
    let pressed = ui.run(t1.view(), [press()]);
    assert_eq!(volumes(&pressed).len(), 1, "the press sets t1's level");
    let after = ui.run(t2.view(), [move_to(at.x - 20.0, at.y), release()]);
    assert!(volumes(&after).is_empty(), "{after:?}");

    // The same take, still held, keeps its drag.
    let mut ui = self::ui();
    ui.point_at(at);
    let _ = ui.run(t1.view(), [press()]);
    let after = ui.run(t1.view(), [move_to(at.x - 20.0, at.y), release()]);
    assert_eq!(volumes(&after).len(), 1);
    assert_eq!(after.last(), Some(&Message::CommitTrackVolume));
}

#[test]
fn a_selected_take_renders_its_channel_strip() {
    let vm = Vm::new().track(vocal("t1")).buses(vec![bus("a", "Reverb")]);
    let mut ui = ui();
    assert_eq!(count(&mut ui, &vm, "Output"), 1);
    // Its header: the name, and what it is (the number left out: the name
    // already reads it).
    assert_eq!(count(&mut ui, &vm, "Take 1"), 1);
    assert_eq!(count(&mut ui, &vm, "Interpretation 1 · 0:05 · Jan 1"), 1);
    // The footer.
    assert_eq!(count(&mut ui, &vm, "0.0 dB"), 1);
}

#[test]
fn a_backing_strip_plays_dry_and_routes_nowhere() {
    let mut backing = vocal("b");
    backing.kind = AudioTrackKind::Accompaniment;
    let vm = Vm::new().track(backing).buses(vec![bus("a", "Reverb")]);
    let mut ui = ui();
    assert_eq!(
        count(&mut ui, &vm, "Backing track plays dry, with no effects."),
        1
    );
    assert_eq!(count(&mut ui, &vm, "Output"), 0);
    assert_eq!(count(&mut ui, &vm, "M"), 1);
}

// ---------------------------------------------------------------- aux sends

fn send_levels(messages: &[Message]) -> Vec<f64> {
    messages
        .iter()
        .filter_map(|m| match m {
            Message::SetTrackSendLevelLive { level_db, .. } => Some(*level_db),
            _ => None,
        })
        .collect()
}

#[test]
fn a_send_pulled_all_the_way_down_is_off_not_at_minus_48_db() {
    // The bottom sent at -48 dB under a "-∞" label: still feeding the bus.
    let vm = Vm::new()
        .track(with_sends(vocal("t1"), &["a"]))
        .buses(vec![bus("a", "Reverb")]);
    let mut ui = ui();
    let target = bounds(&mut ui, &vm, "Reverb");
    // The level's slider is under the target's pop-up (a 22 px row, then a
    // 24 px one), from the 62 px label column and its 8 px gap.
    let left = 14.0 + 62.0 + 8.0;
    let y = target.center_y() + 11.0 + 12.0;
    ui.point_at(Point::new(left + 1.0, y));
    let messages = ui.run(vm.view(), [press(), move_to(left - 39.0, y), release()]);
    let levels = send_levels(&messages);
    assert!(!levels.is_empty(), "{messages:?}");
    assert_eq!(*levels.last().unwrap(), SEND_OFF_DB);
    assert_eq!(messages.last(), Some(&Message::CommitFx));
}

#[test]
fn the_add_menu_offers_only_buses_no_slot_already_feeds() {
    let mut vm = Vm::new().track(with_sends(vocal("t1"), &["a"])).buses(vec![
        bus("a", "Reverb"),
        bus("b", "Delay"),
        bus("c", "Room"),
    ]);
    let mut ui = ui();
    // Slot 0's chip already shows "Reverb", so count occurrences rather than
    // absence: if the menu offered it too there would be a second one.
    assert_eq!(count(&mut ui, &vm, "Reverb"), 1);
    let add = InspectorMenu::AddSend(Channel::Track("t1".into()));
    // The empty slot's pop-up adds a send.
    assert_eq!(
        tap(&mut ui, &vm, EMPTY_SLOT),
        vec![Message::OpenMenu(add.clone())]
    );
    vm.open_menu = Some(add);

    assert_eq!(
        count(&mut ui, &vm, "Reverb"),
        1,
        "still only the chip — bus a is already fed by slot 0"
    );
    assert_eq!(count(&mut ui, &vm, "Delay"), 1);
    assert_eq!(count(&mut ui, &vm, "Room"), 1);

    assert_eq!(
        tap(&mut ui, &vm, "Delay"),
        vec![Message::AddTrackSend {
            track_id: "t1".into(),
            bus_id: "b".into(),
        }]
    );
}

#[test]
fn the_retarget_menu_offers_this_slots_own_target_plus_the_unused_ones() {
    let mut vm = Vm::new()
        .track(with_sends(vocal("t1"), &["a", "b"]))
        .buses(vec![
            bus("a", "Reverb"),
            bus("b", "Delay"),
            bus("c", "Room"),
        ]);
    let mut ui = ui();
    // Slot 0's chip carries its current target's name; slot 1's carries
    // "Delay". Nothing else is on screen yet.
    assert_eq!(count(&mut ui, &vm, "Reverb"), 1);
    assert_eq!(count(&mut ui, &vm, "Delay"), 1);
    assert_eq!(count(&mut ui, &vm, "Room"), 0);

    let retarget = InspectorMenu::RetargetSend(Channel::Track("t1".into()), 0);
    assert_eq!(
        tap(&mut ui, &vm, "Reverb"),
        vec![Message::OpenMenu(retarget.clone())]
    );
    vm.open_menu = Some(retarget);

    assert_eq!(
        count(&mut ui, &vm, "Reverb"),
        2,
        "the chip plus its own target as an option"
    );
    assert_eq!(
        count(&mut ui, &vm, "Delay"),
        1,
        "still only slot 1's chip — a bus another slot feeds is never offered"
    );
    assert_eq!(count(&mut ui, &vm, "Room"), 1, "unused, so offered");

    assert_eq!(
        tap(&mut ui, &vm, "Room"),
        vec![Message::SetTrackSendTarget {
            track_id: "t1".into(),
            slot: 0,
            bus_id: "c".into(),
        }]
    );
}

#[test]
fn removing_a_send_passes_the_slot_index() {
    let vm = Vm::new()
        .track(with_sends(vocal("t1"), &["a", "b"]))
        .buses(vec![bus("a", "Reverb"), bus("b", "Delay")]);
    let mut ui = ui();
    // One remove affordance per slot, in slot order.
    assert_eq!(count(&mut ui, &vm, REMOVE_SEND), 2);
    assert_eq!(
        tap_nth(&mut ui, &vm, REMOVE_SEND, 1),
        vec![Message::RemoveTrackSend {
            track_id: "t1".into(),
            slot: 1,
        }]
    );
}

#[test]
fn the_add_affordance_disappears_at_the_send_cap() {
    let ids: Vec<String> = (0..MAX_SENDS).map(|i| format!("b{i}")).collect();
    let buses: Vec<Arc<FxBus>> = ids.iter().map(|b| bus(b, &format!("Bus {b}"))).collect();
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    let vm = Vm::new()
        .track(with_sends(vocal("t1"), &refs))
        .buses(buses.clone());
    assert_eq!(count(&mut ui(), &vm, EMPTY_SLOT), 0);
    // A slot short of the cap, the empty slot offers the bus left.
    let vm = Vm::new()
        .track(with_sends(vocal("t1"), &refs[1..]))
        .buses(buses);
    assert_eq!(count(&mut ui(), &vm, EMPTY_SLOT), 1);
}

// ------------------------------------------------------ take vs bus dispatch

#[test]
fn mute_and_solo_on_a_take_reach_the_track_methods() {
    let vm = Vm::new().track(vocal("t1"));
    let mut ui = ui();
    assert_eq!(
        tap(&mut ui, &vm, "M"),
        vec![Message::ToggleMute("t1".into())]
    );
    assert_eq!(
        tap(&mut ui, &vm, "S"),
        vec![Message::ToggleSolo("t1".into())]
    );
}

#[test]
fn mute_and_solo_on_a_bus_reach_the_bus_methods() {
    let a = bus("a", "Reverb");
    let vm = Vm::new().bus(a.clone()).buses(vec![a]);
    let mut ui = ui();
    assert_eq!(
        tap(&mut ui, &vm, "M"),
        vec![Message::ToggleBusMute("a".into())]
    );
    assert_eq!(
        tap(&mut ui, &vm, "S"),
        vec![Message::ToggleBusSolo("a".into())]
    );
}

#[test]
fn a_buss_pan_and_fader_commit_its_fx_and_a_takes_its_volume() {
    let a = bus("a", "Reverb");
    let vm = Vm::new().bus(a.clone()).buses(vec![a]);
    let mut ui = ui();
    let at = volume_fader(&mut ui, &vm);
    ui.point_at(at);
    let messages = ui.run(vm.view(), [press(), move_to(at.x + 5.0, at.y), release()]);
    assert!(matches!(
        messages.first(),
        Some(Message::SetBusVolumeLive { bus_id, .. }) if bus_id == "a"
    ));
    assert_eq!(messages.last(), Some(&Message::CommitFx));
}

// ----------------------------------------------------------- menu placement

// The send chips and the bus OUTPUT chip opened their menus from an
// ANCESTOR's context, so the menu dropped below the whole sends section /
// footer (or at its left edge) instead of under the chip that was clicked.
#[test]
fn the_add_send_menu_opens_under_its_chip() {
    let mut vm = Vm::new()
        .track(with_sends(vocal("t1"), &["a", "b"]))
        .buses(vec![
            bus("a", "Reverb"),
            bus("b", "Delay"),
            bus("c", "Room"),
        ]);
    let mut ui = ui();
    let chip = bounds(&mut ui, &vm, EMPTY_SLOT);
    vm.open_menu = Some(InspectorMenu::AddSend(Channel::Track("t1".into())));
    let item = bounds(&mut ui, &vm, "Room");
    assert!(item.y - (chip.y + chip.height) < 30.0, "{chip:?} {item:?}");
    assert!(item.y > chip.y + chip.height);
}

#[test]
fn a_buss_output_menu_opens_under_its_chip() {
    // A panel shorter than the window, so the menu has room below it.
    let mut vm = Vm::new()
        .bus(bus("a", "Reverb"))
        .buses(vec![bus("a", "Reverb"), bus("b", "Delay")])
        .height(900.0);
    let mut ui = ui();
    let chip = bounds(&mut ui, &vm, STEREO_OUT);
    vm.open_menu = Some(InspectorMenu::Output(Channel::Bus("a".into())));
    // The menu's first row repeats the chip's label.
    let item = nth(&mut ui, &vm, STEREO_OUT, 1);
    assert!((item.x - chip.x).abs() < 30.0, "{chip:?} {item:?}");
    assert!(item.y - (chip.y + chip.height) < 30.0);
    assert!(item.y > chip.y + chip.height);
}

#[test]
fn a_menu_for_another_channel_is_not_shown_over_this_one() {
    let mut vm = Vm::new().track(vocal("t1")).buses(vec![bus("a", "Reverb")]);
    vm.open_menu = Some(InspectorMenu::Output(Channel::Track("t0".into())));
    assert_eq!(count(&mut ui(), &vm, STEREO_OUT), 1, "only the chip");
}

#[test]
fn a_tap_outside_the_open_menu_dismisses_it() {
    let mut vm = Vm::new().track(vocal("t1")).buses(vec![bus("a", "Reverb")]);
    vm.open_menu = Some(InspectorMenu::Output(Channel::Track("t1".into())));
    let mut ui = ui();
    ui.point_at(Point::new(800.0, 300.0));
    let messages = ui.run(vm.view(), iced_test::simulator::click());
    assert_eq!(messages, vec![Message::DismissMenu]);
    assert!(messages[0].closes_menu());
    assert!(!Message::OpenMenu(InspectorMenu::AddSend(Channel::Bus("a".into()))).closes_menu());
}

// ----------------------------------------------------------- output routing

#[test]
fn a_take_can_be_routed_to_a_bus_and_back_to_the_master() {
    let mut vm = Vm::new().track(vocal("t1")).buses(vec![bus("a", "Reverb")]);
    let mut ui = ui();
    let output = InspectorMenu::Output(Channel::Track("t1".into()));
    assert_eq!(
        tap(&mut ui, &vm, STEREO_OUT),
        vec![Message::OpenMenu(output.clone())]
    );
    vm.open_menu = Some(output);
    assert_eq!(
        tap(&mut ui, &vm, "Reverb"),
        vec![Message::SetTrackBus {
            track_id: "t1".into(),
            bus_id: Some("a".into()),
        }]
    );
    assert_eq!(
        tap_nth(&mut ui, &vm, STEREO_OUT, 1),
        vec![Message::SetTrackBus {
            track_id: "t1".into(),
            bus_id: None,
        }]
    );
}

// A bus name has no length cap, and the chip sat after a Spacer with no
// bound: a long name ran the chip past the inspector's edge.
const LONG: &str = "A return bus with a name well past the chip";

fn expect_inside_panel(vm: &Vm) {
    let name = bounds(&mut ui(), vm, LONG);
    assert!(name.x + name.width <= CONTENT_W - 14.0 + 0.5, "{name:?}");
}

#[test]
fn a_long_bus_name_stays_inside_a_takes_output_row() {
    let mut t = vocal("t1");
    t.bus_id = Some("a".into());
    expect_inside_panel(&Vm::new().track(t).buses(vec![bus("a", LONG)]));
}

#[test]
fn a_long_bus_name_stays_inside_a_buss_output_row() {
    let routed = Arc::new(FxBus {
        output_bus_id: Some("a".into()),
        ..FxBus::new("b", "Delay", TrackFx::flat())
    });
    expect_inside_panel(
        &Vm::new()
            .bus(routed.clone())
            .buses(vec![bus("a", LONG), routed]),
    );
}

/// Bounding the chip with a Flexible beside the row's Spacer split the free
/// space in half: the chip came off the right edge, and ordinary names
/// ellipsized with most of the row empty. The pop-up fills its column: its
/// name starts 8 px in and shows whole, clear of the chevron.
fn expect_whole_in_its_popup(vm: &Vm, name: &str) {
    let mut ui = ui();
    let shown = bounds(&mut ui, vm, name);
    let natural = iced_test::Simulator::<(), Theme, Renderer>::with_size(
        Settings::default(),
        WINDOW,
        text(name.to_string()).size(11),
    )
    .find(name)
    .expect("the name")
    .bounds();
    assert!(
        (shown.width - natural.width).abs() < 0.5,
        "\"{name}\" shows whole: {shown:?} vs {natural:?}"
    );
    let popup_left = 14.0 + PARAMETER_LABEL_WIDTH + 8.0;
    let popup_right = CONTENT_W - 14.0 - PARAMETER_VALUE_WIDTH - 8.0;
    assert!((shown.x - (popup_left + 8.0)).abs() < 0.5, "{shown:?}");
    // The chevron: 10 px, 6 px in from the right, 8 px after the name.
    assert!(shown.x + shown.width <= popup_right - 6.0 - 10.0 - 8.0 + 0.5);
}

#[test]
fn a_takes_output_popup_shows_its_name_whole() {
    let mut t = vocal("t1");
    t.bus_id = Some("a".into());
    expect_whole_in_its_popup(
        &Vm::new().track(t).buses(vec![bus("a", "Vocal plate")]),
        "Vocal plate",
    );
}

#[test]
fn a_buss_output_popup_shows_its_name_whole() {
    let routed = Arc::new(FxBus {
        output_bus_id: Some("a".into()),
        ..FxBus::new("b", "Delay", TrackFx::flat())
    });
    expect_whole_in_its_popup(
        &Vm::new()
            .bus(routed.clone())
            .buses(vec![bus("a", "Vocal plate"), routed]),
        "Vocal plate",
    );
}

#[test]
fn a_buss_own_output_never_offers_itself() {
    let mut vm = Vm::new()
        .bus(bus("a", "Reverb"))
        .buses(vec![bus("a", "Reverb"), bus("b", "Delay")]);
    vm.open_menu = Some(InspectorMenu::Output(Channel::Bus("a".into())));
    let mut ui = ui();
    // Only the other bus is a legal destination; routing a bus into itself
    // is a feedback loop the menu must not even offer.
    assert_eq!(count(&mut ui, &vm, "Delay"), 1);
    assert_eq!(
        count(&mut ui, &vm, "Reverb"),
        1,
        "the bus's own header only — never itself as a destination"
    );
    assert_eq!(
        tap(&mut ui, &vm, "Delay"),
        vec![Message::SetBusOutput {
            bus_id: "a".into(),
            output_bus_id: Some("b".into()),
        }]
    );
}

// Regression guard: the inspector built its FxPanel without a key, so
// selecting another take (or bus) reused the panel's element tree and the
// EQ's live state — the analyzer's smoothed spectrum, adaptive ceiling and
// latched "has signal" flag — carried over from the previous channel. The
// FX panel's stand-in here is a fader: its held drag is widget state.

#[derive(Debug, Clone, PartialEq)]
enum Fx {
    Level(f64),
    End,
}

/// The panel with an "FX" caption over a 200 px fader standing in for the
/// FX panel; its messages come out as `ShowDockTab(Mixer)` so the panel's
/// message type stays the inspector's.
fn fx_view(vm: &Vm) -> Element<'_, Message> {
    vm.view_with(|| {
        iced::widget::column![
            text("FX"),
            container(
                studio_fader(0.5, Fx::Level, Fx::End).map(|_| Message::ShowDockTab(DockTab::Mixer))
            )
            .width(200),
        ]
        .height(Length::Fill)
        .into()
    })
}

fn fresh_fx_state(before: &Vm, after: &Vm) {
    let mut ui = ui();
    let caption = texts(&mut ui, fx_view(before))
        .into_iter()
        .find(|(c, _, _)| c == "FX")
        .expect("the FX slot is filled")
        .1;
    let at = Point::new(caption.x + 100.0, caption.y + caption.height + 8.0);
    ui.point_at(at);
    let pressed = ui.run(fx_view(before), [press()]);
    assert_eq!(pressed.len(), 1, "the press reaches the FX panel");
    let moved = ui.run(fx_view(after), [move_to(at.x + 30.0, at.y), release()]);
    assert!(moved.is_empty(), "{moved:?}");

    // The same channel keeps its panel's state.
    let mut ui = self::ui();
    ui.point_at(at);
    let _ = ui.run(fx_view(before), [press()]);
    let moved = ui.run(fx_view(before), [move_to(at.x + 30.0, at.y), release()]);
    assert_eq!(moved.len(), 2, "{moved:?}");
}

#[test]
fn selecting_another_take_gives_the_fx_panel_fresh_state() {
    fresh_fx_state(&Vm::new().track(vocal("t1")), &Vm::new().track(vocal("t2")));
}

#[test]
fn selecting_another_bus_gives_the_fx_panel_fresh_state() {
    let (a, b) = (bus("a", "Reverb"), bus("b", "Delay"));
    fresh_fx_state(
        &Vm::new().bus(a.clone()).buses(vec![a.clone(), b.clone()]),
        &Vm::new().bus(b.clone()).buses(vec![a, b]),
    );
}

// --------------------------------------------------------------- the rail

#[test]
fn the_rail_and_the_header_toggle_the_inspector() {
    let vm = Vm::new().track(vocal("t1"));
    let closed = Inspector {
        open: false,
        ..vm.inspector()
    };
    let mut sim = iced_test::simulator(left_panel(closed, || Space::new().into(), id));
    sim.point_at(Point::new(17.0, 300.0));
    let _ = sim.simulate(iced_test::simulator::click());
    assert_eq!(
        sim.into_messages().collect::<Vec<_>>(),
        vec![Message::ToggleInspectorPanel]
    );
    let mut ui = ui();
    assert_eq!(count(&mut ui, &vm, "Inspector"), 1);
    assert_eq!(
        tap(&mut ui, &vm, "Hide inspector"),
        vec![Message::ToggleInspectorPanel]
    );
}

// ----------------------------------------------------------- the summary

const SR: i64 = 48000;
const HOP: i64 = 512;

/// Hand-built metrics: `f0` per frame (NaN unvoiced).
fn metrics_with(frames: usize, f0: impl Fn(usize) -> f64) -> Arc<VoiceMetrics> {
    let f0: Vec<f32> = (0..frames).map(|i| f0(i) as f32).collect();
    let nan = vec![f32::NAN; frames];
    Arc::new(VoiceMetrics {
        frames,
        hop: HOP,
        fft_size: 2048,
        sample_rate: SR,
        ring: vec![0.0; frames],
        tilt: vec![0.0; frames],
        cpp: vec![0.0; frames],
        loudness: vec![-20.0; frames],
        active: f0.iter().map(|hz| u8::from(hz.is_finite())).collect(),
        f0,
        f1: nan.clone(),
        f2: nan.clone(),
        f3: nan.clone(),
        h1h2: nan,
        ring_avg: 0.0,
        tilt_avg: 0.0,
        cpp_avg: 12.345,
        peak_dbfs: f64::NAN,
        clipped_fraction: 0.0,
        snr_db: f64::NAN,
    })
}

/// A sung note at `hz`, `cents` off and wobbling `wobble` cents at 5.5 Hz.
fn sung(hz: f64, cents: f64, wobble: f64, frames: usize) -> Arc<VoiceMetrics> {
    let fps = SR as f64 / HOP as f64;
    metrics_with(frames, |i| {
        if i < 5 || i + 5 >= frames {
            return f64::NAN;
        }
        let c = cents + wobble * (2.0 * PI * 5.5 * i as f64 / fps).sin();
        hz * pow_f64(2.0, c / 1200.0)
    })
}

fn profile() -> VoiceProfile {
    VoiceProfile {
        low_midi: Some(57),
        high_midi: Some(76),
        tess_lo_midi: Some(60),
        tess_hi_midi: Some(70),
        passaggio_midi: Some(64),
        passaggio_jump_db: 3.0,
        soft_db: -32.0,
        loud_db: -11.6,
        median_db: -20.0,
    }
}

fn register() -> RegisterMap {
    RegisterMap {
        score: Vec::new(),
        chest_pct: 40.4,
        mix_pct: 35.0,
        head_pct: 24.6,
        voiced_frames: 100,
    }
}

fn ltas(singers_formant_db: f64, alpha_ratio: f64) -> Ltas {
    Ltas {
        db: vec![-40.0; 8],
        bin_hz: 100.0,
        max_hz: 800.0,
        max_db: -10.0,
        alpha_ratio,
        singers_formant_db,
        frame_count: 10,
    }
}

fn summary<'a>(track: &'a AudioTrack) -> VoiceSummary<'a> {
    VoiceSummary {
        selected_track: Some(track),
        spectrogram_loading: false,
        metrics_loading: false,
        metrics: None,
        profile: None,
        register: None,
        ltas: None,
    }
}

fn summary_texts(s: VoiceSummary<'_>) -> Vec<String> {
    let mut ui = iced_test::Simulator::with_size(
        Settings::default(),
        Size::new(PANEL_WIDTH * 2.0, 600.0),
        voice_summary_view(s),
    );
    let mut found = Vec::new();
    let _ = ui.find(|candidate: Candidate<'_>| {
        if let Candidate::Text { content, .. } = candidate {
            found.push(content.to_string());
        }
        None::<()>
    });
    found
}

#[test]
fn the_summary_says_why_it_shows_nothing() {
    let take = vocal("t1");
    let none = VoiceSummary {
        selected_track: None,
        ..summary(&take)
    };
    assert!(summary_texts(none).contains(&"Select a take to see its analysis.".to_string()));
    let mut backing = vocal("b");
    backing.kind = AudioTrackKind::Reference;
    assert!(summary_texts(summary(&backing))
        .contains(&"Backing track has no voice analysis.".to_string()));
    let loading = VoiceSummary {
        metrics_loading: true,
        ..summary(&take)
    };
    assert!(summary_texts(loading).contains(&"Analyzing…".to_string()));
    assert!(summary_texts(summary(&take)).contains(&"No analysis for this take.".to_string()));
}

#[test]
fn an_in_tune_straight_take_reads_as_such() {
    let take = vocal("t1");
    let m = sung(220.0, 0.0, 0.0, 120);
    let p = profile();
    let l = ltas(8.4, -12.5);
    let reg = register();
    let cells = summary_cells(&m, &p, Some(&l));
    let shown: Vec<(&str, &str, &str)> = cells
        .iter()
        .map(|c| (c.label, c.value.as_str(), c.sub.as_str()))
        .collect();
    assert_eq!(
        shown,
        vec![
            ("In tune", "100%", "of notes"),
            ("Vibrato", "straight", "no vibrato"),
            ("Clarity", "12.3", "CPP"),
            ("Ring", "8 dB", "singer's formant"),
            ("Dynamics", "20 dB", "soft → loud"),
            ("Brightness", "-13 dB", "α ratio"),
        ]
    );
    assert_eq!(cells[0].dot, Some(GOOD));
    assert_eq!(cells[1].dot, None);
    let tabs: Vec<DockTab> = cells.iter().map(|c| c.tab).collect();
    assert_eq!(
        tabs,
        vec![
            DockTab::Pitch,
            DockTab::Vibrato,
            DockTab::Timbre,
            DockTab::Ltas,
            DockTab::Dynamics,
            DockTab::Ltas,
        ]
    );

    let s = VoiceSummary {
        metrics: Some(&m),
        profile: Some(&p),
        register: Some(&reg),
        ltas: Some(&l),
        ..summary(&take)
    };
    let shown = summary_texts(s);
    for t in [
        "Voice summary",
        "A3 – E5",
        "19 st",
        "Range",
        "In tune",
        "100%",
        "Register",
        "chest 40 · mix 35 · head 25",
    ] {
        assert!(shown.contains(&t.to_string()), "{t} in {shown:?}");
    }
}

#[test]
fn a_sharp_take_is_flagged() {
    let m = sung(220.0, 35.0, 0.0, 120);
    let cells = summary_cells(&m, &profile(), None);
    assert_eq!(cells[0].value, "0%");
    assert_eq!(cells[0].dot, Some(FLAG));
    assert_eq!(cells[1].value, "straight");
}

#[test]
fn the_vibrato_is_read_off_the_longest_note() {
    let m = sung(220.0, 0.0, 40.0, 400);
    let cells = summary_cells(&m, &profile(), None);
    let notes = pitch_notes_of(&m);
    let v = longest_note_vibrato(&m, &notes).expect("a note");
    assert!(v.present, "{v:?}");
    assert_eq!(
        cells[1].value,
        format!("{} Hz", to_string_as_fixed(v.rate_hz, 1))
    );
    assert_eq!(cells[1].sub, format!("±{}¢", round_to_i64(v.extent_cents)));
    let expected = if (5.0..=7.0).contains(&v.rate_hz) {
        GOOD
    } else {
        WARN
    };
    assert_eq!(cells[1].dot, Some(expected));
    // No LTAS: the ring and brightness have no reading.
    assert_eq!(cells[3].value, "—");
    assert_eq!(cells[5].value, "—");
}

#[test]
fn a_take_with_no_notes_or_dynamics_reads_dashes() {
    let m = metrics_with(40, |_| f64::NAN);
    let p = VoiceProfile {
        low_midi: None,
        high_midi: None,
        soft_db: f64::NAN,
        ..profile()
    };
    let mut nan_ltas = ltas(f64::NAN, f64::INFINITY);
    let cells = summary_cells(&m, &p, Some(&nan_ltas));
    assert_eq!(cells[0].value, "—");
    assert_eq!(cells[0].dot, None);
    assert_eq!(cells[1].value, "straight");
    assert_eq!(cells[3].value, "—", "a non-finite reading is no reading");
    assert_eq!(cells[4].value, "—");
    assert_eq!(cells[5].value, "—");
    nan_ltas.frame_count = 0;
    assert_eq!(summary_cells(&m, &p, Some(&nan_ltas))[3].value, "—");
    assert_eq!(range_line(&p), None);
}

#[test]
fn a_tap_on_a_cell_opens_its_dock_tab() {
    let take = vocal("t1");
    let m = sung(220.0, 0.0, 0.0, 120);
    let p = profile();
    let reg = register();
    let s = VoiceSummary {
        metrics: Some(&m),
        profile: Some(&p),
        register: Some(&reg),
        ..summary(&take)
    };
    for (label, tab) in [
        ("In tune", DockTab::Pitch),
        ("Clarity", DockTab::Timbre),
        ("Range", DockTab::Range),
        ("Register", DockTab::Registers),
        ("Dynamics", DockTab::Dynamics),
    ] {
        let mut ui = iced_test::simulator(voice_summary_view(s));
        ui.click(label).expect("on the summary");
        assert_eq!(
            ui.into_messages().collect::<Vec<_>>(),
            vec![Message::ShowDockTab(tab)],
            "{label}"
        );
    }
}

#[test]
fn a_clipped_or_noisy_take_is_flagged_above_the_summary() {
    let mut m = (*sung(220.0, 0.0, 0.0, 60)).clone();
    m.clipped_fraction = 0.0123;
    m.snr_db = 4.0;
    assert_eq!(
        quality_warning(&m).as_deref(),
        Some("Clipped 1.2% — lower the mic gain and re-record.")
    );
    m.clipped_fraction = 0.0;
    assert_eq!(
        quality_warning(&m).as_deref(),
        Some("Low SNR (4 dB) — record closer to the mic / quieter.")
    );
    m.snr_db = 30.0;
    assert_eq!(quality_warning(&m), None);

    let take = vocal("t1");
    m.snr_db = 4.0;
    let m = Arc::new(m);
    let p = profile();
    let s = VoiceSummary {
        metrics: Some(&m),
        profile: Some(&p),
        ..summary(&take)
    };
    assert!(summary_texts(s)
        .contains(&"Low SNR (4 dB) — record closer to the mic / quieter.".to_string()));
}

// ----------------------------------------------------------------- the pure

// The subtitle numbered a take by its place: reordered, "Take 2" read
// "Take 3 · Interpretation 1" under its name.
#[test]
fn a_takes_subtitle_is_its_interpretation_length_and_date_with_no_number() {
    let mut original = vocal("r");
    original.kind = AudioTrackKind::Reference;
    assert_eq!(
        take_subtitle(&original, Some("Tender")),
        "Original · 0:05 · Jan 1"
    );
    original.kind = AudioTrackKind::Accompaniment;
    assert!(take_subtitle(&original, None).starts_with("Accompaniment · "));
    let mut take = vocal("t1");
    take.display_name = "Take 2".into();
    assert_eq!(
        take_subtitle(&take, Some("Tender")),
        "Tender · 0:05 · Jan 1"
    );
    assert_eq!(take_subtitle(&take, None), "0:05 · Jan 1");
    take.display_name = "Chorus only".into();
    assert_eq!(
        take_subtitle(&take, Some("Tender")),
        "Tender · 0:05 · Jan 1"
    );
    assert_eq!(short_clock(754_000), "12:34");
    assert_eq!(fmt_date(&DateTime::utc_ymd(2026, 11, 23)), "Nov 23");
}

#[test]
fn a_take_dot_is_its_tint_else_the_lane_default() {
    use crate::ui::song_practice_detail::widgets::track_row::take_color;
    let mut t = vocal("t1");
    assert_eq!(take_dot_color(&t), take_color(&t));
    assert_eq!(take_dot_color(&t), Studio::default_track_color("t1"));
    t.color_value = Some(0xFF5B8DEF);
    assert_eq!(take_dot_color(&t), Color::from_rgb8(0x5B, 0x8D, 0xEF));
    let mut b = vocal("b");
    b.kind = AudioTrackKind::Accompaniment;
    assert_eq!(take_dot_color(&b), Studio::TRACK_BACKING);
}

#[test]
fn the_send_fader_maps_minus_48_to_0_db_and_its_bottom_is_off() {
    assert_eq!(send_level_at(0.0), SEND_OFF_DB);
    assert_eq!(send_level_at(-0.1), SEND_OFF_DB);
    assert_eq!(send_level_at(0.5), -24.0);
    assert_eq!(send_level_at(1.0), 0.0);
    assert_eq!(send_fader_value(0.0), 1.0);
    assert_eq!(send_fader_value(-24.0), 0.5);
    assert_eq!(send_fader_value(SEND_OFF_DB), 0.0);
    assert_eq!(fmt_send_db(SEND_OFF_DB), "-∞");
    assert_eq!(fmt_send_db(-12.5), "-13");
    assert_eq!(fmt_send_db(0.0), "0");
}

#[test]
fn the_offer_sets_never_feed_a_bus_twice() {
    let (a, b, c) = (
        FxBus::new("a", "A", TrackFx::flat()),
        FxBus::new("b", "B", TrackFx::flat()),
        FxBus::new("c", "C", TrackFx::flat()),
    );
    let targets = vec![&a, &b, &c];
    let sends = vec![TrackSend::new("a", 0.0), TrackSend::new("b", 0.0)];
    let ids = |v: Vec<&FxBus>| v.iter().map(|b| b.id.clone()).collect::<Vec<_>>();
    assert_eq!(ids(addable_targets(&sends, &targets)), vec!["c"]);
    assert_eq!(ids(retarget_options(&sends, 0, &targets)), vec!["a", "c"]);
    assert_eq!(ids(retarget_options(&sends, 1, &targets)), vec!["b", "c"]);
    assert!(can_add_send(&sends, &targets));
    assert!(!can_add_send(&sends, &[&a, &b]));
    assert_eq!(send_name("b", &targets), "B");
    assert_eq!(send_name("zz", &targets), "—");
    let buses = vec![Arc::new(a.clone()), Arc::new(b.clone())];
    assert_eq!(output_name(None, &buses), STEREO_OUT);
    assert_eq!(output_name(Some("b"), &buses), "B");
    assert_eq!(output_name(Some("gone"), &buses), STEREO_OUT);
}

#[test]
fn the_register_bar_splits_by_whole_tenths() {
    let reg = register();
    let w = register_widths(&reg, 1000.0);
    // Flexes 404 / 350 / 246.
    assert!((w[0] - 404.0).abs() < 0.01 && (w[1] - 350.0).abs() < 0.01);
    assert!((w.iter().sum::<f32>() - 1000.0).abs() < 0.01);
    let none = RegisterMap {
        chest_pct: 0.0,
        mix_pct: 0.0,
        head_pct: 0.0,
        ..register()
    };
    assert_eq!(register_widths(&none, 100.0), [0.0; 3]);
    assert_eq!(register_line(&reg), "chest 40 · mix 35 · head 25");
    let mix = mix_color();
    assert!((mix.r - (CHEST.r + HEAD.r) / 2.0).abs() < 1e-6);
}

#[test]
fn the_range_bar_maps_the_sung_span() {
    assert_eq!(range_x(57, 57, 76, 190.0), 0.0);
    assert_eq!(range_x(76, 57, 76, 190.0), 190.0);
    assert_eq!(range_x(66, 57, 76, 190.0), 90.0);
    // Clamped, and a one-note range spans a semitone.
    assert_eq!(range_x(90, 57, 76, 190.0), 190.0);
    assert_eq!(range_x(60, 60, 60, 100.0), 0.0);
    assert_eq!(range_line(&profile()), Some(("A3 – E5".to_string(), 19)));
}

#[test]
fn a_metric_card_splits_its_unit_and_sits_its_reading_on_its_bar() {
    assert_eq!(split_unit("86%"), ("86", "%"));
    assert_eq!(split_unit("5.6 Hz"), ("5.6", "Hz"));
    assert_eq!(split_unit("-13 dB"), ("-13", "dB"));
    assert_eq!(split_unit("12.3"), ("12.3", ""));
    assert_eq!(split_unit("straight"), ("straight", ""));
    assert_eq!(split_unit("—"), ("—", ""));
    assert_eq!(split_unit("%"), ("%", ""));
    assert_eq!(metric_level(50.0, (0.0, 100.0)), Some(0.5));
    assert_eq!(metric_level(-15.0, (-30.0, 0.0)), Some(0.5));
    assert_eq!(metric_level(140.0, (0.0, 100.0)), Some(1.0), "clamped");
    assert_eq!(metric_level(f64::NAN, (0.0, 100.0)), None);
    let m = sung(220.0, 0.0, 0.0, 120);
    let cells = summary_cells(&m, &profile(), Some(&ltas(-15.0, -6.0)));
    let levels: Vec<Option<f32>> = cells.iter().map(|c| c.level).collect();
    // In tune 100 %, no vibrato, CPP 12.345 of 20, ring −15 of −30..0,
    // dynamics 20.4 of 40, brightness −6 of −30..0.
    assert_eq!(levels[0], Some(1.0));
    assert_eq!(levels[1], None);
    assert!((levels[2].unwrap() - 0.617).abs() < 0.001);
    assert_eq!(levels[3], Some(0.5));
    assert!((levels[4].unwrap() - 0.51).abs() < 0.001);
    assert_eq!(levels[5], Some(0.8));
    // The cards fill the dock: six a row on a wide one.
    assert_eq!(summary_columns(1400.0), 6);
    assert_eq!(summary_columns(600.0), 3);
    assert_eq!(summary_columns(300.0), 2);
}

#[test]
fn the_whole_strip_draws() {
    let buses = vec![bus("a", "Reverb"), bus("b", "Delay")];
    let mut t = with_sends(vocal("t1"), &["a"]);
    t.bus_id = Some("b".into());
    for vm in [
        Vm::new().track(t.clone()).buses(buses.clone()),
        Vm::new().track(t).buses(buses.clone()).height(200.0),
        Vm::new().bus(buses[0].clone()).buses(buses.clone()),
    ] {
        let mut sim = iced_test::Simulator::with_size(Settings::default(), WINDOW, vm.view());
        sim.snapshot(&theme::app_theme()).expect("snapshot");
    }
}
