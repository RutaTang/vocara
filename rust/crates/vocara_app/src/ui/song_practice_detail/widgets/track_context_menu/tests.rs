//! The menus' tests: `track_context_menu_test.dart`, the tints (against
//! values Flutter's own `HSLColor` produced), and the host's placement and
//! dismissal.

use iced::keyboard::key::Named;
use iced::widget::{column, container, text};
use iced::{keyboard, Settings};
use iced_test::simulator;

use super::*;
use crate::ui::song_practice_detail::widgets::live_ui::{
    move_to, press, release, right_click, LiveUi,
};

fn vocal() -> TrackMenu {
    TrackMenu {
        is_vocal: true,
        ..TrackMenu::default()
    }
}

/// Whether `menu` shows `label`.
fn shows<M: Clone + 'static>(menu: Element<'static, M>, label: &str) -> bool {
    let mut ui = simulator(menu);
    ui.find(label).is_ok()
}

// ---------------------------------------------- track_context_menu_test

#[test]
fn hides_extract_backing_when_the_engine_is_unavailable() {
    // The screen passes canSeparate: track.isReference &&
    // vm.separationAvailable, so an unavailable engine reaches the menu as
    // can_separate: false.
    let menu = TrackMenu::default();
    assert!(!shows(track_context_menu(&menu), "Extract backing"));
    assert!(!shows(track_context_menu(&menu), "Re-extract backing"));
}

#[test]
fn shows_extract_backing_when_the_engine_is_available() {
    let menu = TrackMenu {
        can_separate: true,
        ..TrackMenu::default()
    };
    assert!(shows(track_context_menu(&menu), "Extract backing"));
    let again = TrackMenu {
        reseparate: true,
        ..menu
    };
    assert!(shows(track_context_menu(&again), "Re-extract backing"));
}

#[test]
fn offers_export_take_for_a_vocal_lane_with_audio() {
    let menu = TrackMenu {
        can_export: true,
        ..vocal()
    };
    assert!(shows(track_context_menu(&menu), "Export take…"));
}

#[test]
fn offers_export_audio_for_a_backing_lane_with_audio() {
    let menu = TrackMenu {
        can_export: true,
        ..TrackMenu::default()
    };
    assert!(shows(track_context_menu(&menu), "Export audio…"));
}

#[test]
fn hides_export_for_a_lane_with_no_audio() {
    assert!(!shows(track_context_menu(&vocal()), "Export take…"));
    assert!(!shows(track_context_menu(&vocal()), "Export audio…"));
}

#[test]
fn offers_add_audio_clip_at_playhead_on_a_vocal_lane() {
    // The click-to-upload twin of dropping audio onto the lane.
    let mut ui = simulator(track_context_menu(&vocal()));
    ui.click("Add audio clip at playhead…").expect("the row");
    assert_eq!(
        ui.into_messages().collect::<Vec<_>>(),
        vec![TrackAction::AddClip]
    );
}

#[test]
fn hides_add_audio_clip_on_a_backing_lane() {
    // Clips are a takes feature.
    assert!(!shows(
        track_context_menu(&TrackMenu::default()),
        "Add audio clip at playhead…"
    ));
}

#[test]
fn offers_export_clip_and_runs_the_action() {
    let mut ui = simulator(clip_context_menu(true));
    ui.click("Export clip…").expect("the row");
    assert_eq!(
        ui.into_messages().collect::<Vec<_>>(),
        vec![ClipAction::Export]
    );
}

#[test]
fn omits_export_clip_when_no_export_action_is_wired() {
    assert!(!shows(clip_context_menu(false), "Export clip…"));
    assert!(shows(clip_context_menu(false), "Delete clip"));
}

// ------------------------------------------------------- the rows

#[test]
fn vocal_only_rows_follow_the_lane_kind() {
    let rows = [
        "Add note at playhead",
        "Add technique at playhead",
        "Add audio clip at playhead…",
        "Duplicate",
        "Split at playhead",
    ];
    for row in rows {
        assert!(shows(track_context_menu(&vocal()), row), "{row}");
        assert!(
            !shows(track_context_menu(&TrackMenu::default()), row),
            "{row}"
        );
    }
    // Merge needs a lane below, and a vocal lane.
    let below = TrackMenu {
        can_merge_down: true,
        ..vocal()
    };
    assert!(shows(track_context_menu(&below), "Merge with take below"));
    assert!(!shows(
        track_context_menu(&vocal()),
        "Merge with take below"
    ));
    let backing_below = TrackMenu {
        can_merge_down: true,
        ..TrackMenu::default()
    };
    assert!(!shows(
        track_context_menu(&backing_below),
        "Merge with take below"
    ));
    // The delete row names what goes.
    assert!(shows(track_context_menu(&vocal()), "Delete take"));
    assert!(shows(track_context_menu(&TrackMenu::default()), "Delete"));
    assert!(!shows(
        track_context_menu(&TrackMenu::default()),
        "Delete take"
    ));
    // Always: the color caption, rename and re-analyze.
    for row in ["COLOR", "Rename", "Re-analyze waveform"] {
        assert!(shows(track_context_menu(&vocal()), row), "{row}");
    }
}

#[test]
fn each_row_reports_its_action() {
    let menu = TrackMenu {
        can_merge_down: true,
        can_export: true,
        ..vocal()
    };
    for (label, action) in [
        ("Rename", TrackAction::Rename),
        ("Add note at playhead", TrackAction::AddNote),
        ("Add technique at playhead", TrackAction::AddTechnique),
        ("Duplicate", TrackAction::Duplicate),
        ("Split at playhead", TrackAction::Split),
        ("Merge with take below", TrackAction::MergeDown),
        ("Export take…", TrackAction::Export),
        ("Re-analyze waveform", TrackAction::Reanalyze),
        ("Delete take", TrackAction::Delete),
    ] {
        let mut ui = simulator(track_context_menu(&menu));
        ui.click(label).expect(label);
        assert_eq!(ui.into_messages().collect::<Vec<_>>(), vec![action]);
    }
    let mut ui = simulator(track_context_menu(&TrackMenu {
        can_separate: true,
        ..TrackMenu::default()
    }));
    ui.click("Extract backing").expect("the row");
    assert_eq!(
        ui.into_messages().collect::<Vec<_>>(),
        vec![TrackAction::Separate]
    );
}

#[test]
fn the_bus_menu_offers_color_rename_duplicate_and_delete() {
    for (label, action) in [
        ("Rename", BusAction::Rename),
        ("Duplicate", BusAction::Duplicate),
        ("Delete", BusAction::Delete),
    ] {
        let mut ui = simulator(bus_context_menu(&BusMenu::default()));
        assert!(ui.find("COLOR").is_ok());
        ui.click(label).expect(label);
        assert_eq!(ui.into_messages().collect::<Vec<_>>(), vec![action]);
    }
    for absent in [
        "Split at playhead",
        "Re-analyze waveform",
        "Add note at playhead",
    ] {
        assert!(!shows(bus_context_menu(&BusMenu::default()), absent));
    }
}

/// The track menu in a 1000 × 800 window, at its top-left.
fn menu_ui(menu: &TrackMenu) -> iced_test::Simulator<'static, TrackAction> {
    iced_test::Simulator::with_size(
        Settings::default(),
        Size::new(1000.0, 800.0),
        column![track_context_menu(menu)],
    )
}

#[test]
fn the_color_grid_is_two_rows_of_eight_and_a_swatch_picks_its_tint() {
    // The grid's first swatch sits under the caption, 14 + 1 px in (the
    // padding inside the 1 px border); each next one 30 px on (22 + 8), and
    // the ninth starts the second row.
    let swatch = |i: usize| {
        Point::new(
            1.0 + 14.0 + 11.0 + (i % 8) as f32 * 30.0,
            1.0 + 12.0 + 11.0 + 8.0 + (i / 8) as f32 * 30.0 + 11.0,
        )
    };
    for (i, &value) in TRACK_COLOR_PALETTE.iter().enumerate() {
        let mut ui = menu_ui(&vocal());
        // The caption's line box: 9 px in a 1.3 line height (rounded up by
        // the text layout); probe the swatch's centre band.
        ui.point_at(swatch(i));
        let _ = ui.simulate(iced_test::simulator::click());
        assert_eq!(
            ui.into_messages().collect::<Vec<_>>(),
            vec![TrackAction::PickColor(value)],
            "swatch {i}"
        );
    }
}

#[test]
fn the_menu_is_its_fixed_width() {
    let mut ui = menu_ui(&vocal());
    let rename = ui.find("Rename").expect("the row").bounds();
    // The row's label starts after the 14 px padding, a 15 px icon and a
    // 12 px gap, inside the 1 px border.
    assert!(
        (rename.x - (1.0 + 14.0 + 15.0 + 12.0)).abs() < 0.5,
        "{rename:?}"
    );
    assert_eq!(MENU_WIDTH, 268.0);
}

// ------------------------------------------------------- the studio menu

#[derive(Debug, Clone, PartialEq)]
enum Pick {
    A,
    B,
    New,
}

#[test]
fn the_studio_menu_marks_the_selected_item_and_reports_taps() {
    let items = || {
        vec![
            StudioMenuItem::new("Tender", Pick::A).selected(true),
            StudioMenuItem::new("Powerful", Pick::B).subtitle("Belted"),
            StudioMenuItem::new("New interpretation", Pick::New)
                .icon(Icon::Plus)
                .divider(true),
        ]
    };
    let mut ui = simulator(studio_menu(items(), 224.0));
    assert!(ui.find("Belted").is_ok());
    ui.click("Powerful").expect("the row");
    ui.click("New interpretation").expect("the row");
    assert_eq!(
        ui.into_messages().collect::<Vec<_>>(),
        vec![Pick::B, Pick::New]
    );
    // The selected row reads in the accent; a danger row in red.
    assert_eq!(MenuRow::new("x", ()).selected(true).color(), Studio::ACCENT);
    assert_eq!(
        MenuRow::new("x", ()).selected(true).danger(true).color(),
        Studio::RECORD
    );
    assert_eq!(MenuRow::new("x", ()).color(), Studio::TEXT_PRIMARY);
}

// ------------------------------------------------------------ the prompts

#[test]
fn the_rename_and_delete_bus_prompts_say_what_dart_said() {
    let p = rename_dialog("Take 1");
    assert_eq!(p.title, "Rename track");
    assert_eq!(p.hint, "Track name");
    assert_eq!(p.initial.as_deref(), Some("Take 1"));
    assert_eq!(p.confirm_label, "Rename");
    assert_eq!(p.cancel_label, "Cancel");

    let d = confirm_delete_bus("Verb");
    assert_eq!(d.title, "Delete \"Verb\"?");
    assert_eq!(
        d.body.as_deref(),
        Some("Removing this FX bus re-routes anything feeding it to the master.")
    );
    assert_eq!(d.confirm_label, "Delete");
    assert!(d.destructive);
}

// ---------------------------------------------------------------- tints

/// `0xAARRGGBB` of a color.
fn hex(c: Color) -> u32 {
    let [r, g, b, a] = c.into_rgba8();
    u32::from_be_bytes([a, r, g, b])
}

#[test]
fn argb_reads_dart_color_ints() {
    assert_eq!(hex(argb(0xFFE3604E)), 0xFFE3604E);
    assert_eq!(argb(0xFFE07A6A), Studio::ACCENT);
    // Only the low 32 bits count.
    assert_eq!(hex(argb(0x1_FFE3604E)), 0xFFE3604E);
    assert_eq!(hex(argb(0x80FF0000)), 0x80FF0000);
}

#[test]
fn a_take_keeps_its_default_tint_by_the_sum_of_its_code_units() {
    // Indices Dart computed (`codeUnits.fold(0, +) % 16`).
    for (id, index) in [
        ("t1", 5),
        ("take-01HZX", 13),
        ("abc", 6),
        ("", 0),
        ("é漢", 11),
    ] {
        assert_eq!(
            default_take_color(id),
            argb(TRACK_COLOR_PALETTE[index]),
            "{id}"
        );
    }
}

#[test]
fn the_selected_border_lifts_the_lane_tint_as_flutter_did() {
    // `selectedLaneColor` run by Flutter's HSLColor on these inputs.
    let cases: [(i64, u32); 21] = [
        (0xFFE3604E, 0xFFF09588),
        (0xFFE2934A, 0xFFEFB884),
        (0xFFD9B44A, 0xFFE9CE81),
        (0xFFB7C44A, 0xFFD0DA7C),
        (0xFF6FB562, 0xFF99D08F),
        (0xFF49B6A8, 0xFF77CFC4),
        (0xFF4AAEC4, 0xFF7CC9DA),
        (0xFF5B8DEF, 0xFF99B9F8),
        (0xFF6F7CE8, 0xFFAAB2F4),
        (0xFF8C7CE8, 0xFFC0B7F5),
        (0xFFB06FE0, 0xFFD1A7EF),
        (0xFFD46BA8, 0xFFE79FC9),
        (0xFFD46172, 0xFFE796A2),
        (0xFFB57C5A, 0xFFCFA287),
        (0xFF8A93A8, 0xFFB0B7CA),
        (0xFF7FB0A0, 0xFFA8CFC2),
        // The backing's accent, and greys (hue 0: the saturation lift
        // tints them red, as in Flutter).
        (0xFFE07A6A, 0xFFEFADA2),
        (0xFF000000, 0xFF252222),
        (0xFFFFFFFF, 0xFFFFFFFF),
        (0xFF808080, 0xFFA89F9F),
        (0xFFF0F0F0, 0xFFFFFFFF),
    ];
    for (base, lifted) in cases {
        assert_eq!(hex(selected_lane_color(argb(base))), lifted, "{base:#x}");
    }
}

// ------------------------------------------------------------- the host

#[test]
fn a_menu_opens_at_the_press_and_stays_inside_the_window() {
    let window = Size::new(1000.0, 700.0);
    let menu = Size::new(268.0, 400.0);
    assert_eq!(
        menu_position(Point::new(40.0, 40.0), menu, window),
        Point::new(40.0, 40.0)
    );
    // Past the right and bottom edges: pulled back inside the margin.
    assert_eq!(
        menu_position(Point::new(900.0, 600.0), menu, window),
        Point::new(1000.0 - 268.0 - 8.0, 700.0 - 400.0 - 8.0)
    );
    // Above or left of the margin: held at it.
    assert_eq!(
        menu_position(Point::new(2.0, -5.0), menu, window),
        Point::new(8.0, 8.0)
    );
    // Taller than the window: held at the top margin.
    assert_eq!(
        menu_position(Point::new(10.0, 10.0), Size::new(268.0, 900.0), window).y,
        8.0
    );
    // A window narrower than the margin (Dart's clamp threw).
    assert_eq!(
        menu_position(Point::new(10.0, 10.0), menu, Size::new(4.0, 4.0)),
        Point::new(8.0, 8.0)
    );
}

#[derive(Debug, Clone, PartialEq)]
enum Host {
    Open,
    Dismiss,
    Menu(TrackAction),
}

/// A 200 × 60 target at (100, 50) opening on a right-click, with the track
/// menu open when `open`.
fn host_view(open: bool) -> Element<'static, Host> {
    let target = iced::widget::mouse_area(
        container(text("target"))
            .width(200)
            .height(60)
            .style(theme::fill(Studio::PANEL)),
    )
    .on_right_press(Host::Open);
    let menu = open.then(|| track_context_menu(&vocal()).map(Host::Menu));
    container(menu_host(target, menu, Host::Dismiss))
        .padding(Padding {
            top: 50.0,
            left: 100.0,
            ..Padding::ZERO
        })
        .into()
}

#[test]
fn the_host_pops_the_menu_where_it_was_right_clicked() {
    let mut ui = LiveUi::new(Size::new(1000.0, 800.0));
    ui.point_at(Point::new(150.0, 70.0));
    assert_eq!(ui.run(host_view(false), right_click()), vec![Host::Open]);
    // The screen opens the menu: its card's top-left is at the press (the
    // first row's label 1 + 14 + 15 + 12 px in from it).
    let rename = ui
        .find(host_view(true), "Rename")
        .expect("the menu is open")
        .bounds();
    assert!((rename.x - (150.0 + 42.0)).abs() < 0.5, "{rename:?}");
    assert!(rename.y > 70.0 && rename.y < 70.0 + 140.0, "{rename:?}");
    // A row reports its action.
    ui.point_at(rename.center());
    assert_eq!(
        ui.run(host_view(true), iced_test::simulator::click()),
        vec![Host::Menu(TrackAction::Rename)]
    );
}

#[test]
fn a_menu_opened_near_the_corner_is_pulled_inside_the_window() {
    let mut ui = LiveUi::new(Size::new(1000.0, 800.0));
    let view = |open: bool| {
        let target = iced::widget::mouse_area(
            container(text("target"))
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .on_right_press(Host::Open);
        let menu = open.then(|| track_context_menu(&vocal()).map(Host::Menu));
        Element::from(menu_host(target, menu, Host::Dismiss))
    };
    ui.point_at(Point::new(990.0, 790.0));
    assert_eq!(ui.run(view(false), right_click()), vec![Host::Open]);
    let rename = ui.find(view(true), "Rename").expect("open").bounds();
    assert!(
        (rename.x - (1000.0 - 8.0 - MENU_WIDTH + 42.0)).abs() < 0.5,
        "{rename:?}"
    );
    let delete = ui.find(view(true), "Delete take").expect("open").bounds();
    assert!(delete.y + delete.height < 800.0 - 8.0, "{delete:?}");
}

#[test]
fn a_tap_outside_or_escape_dismisses_and_nothing_beneath_sees_it() {
    let mut ui = LiveUi::new(Size::new(1000.0, 800.0));
    ui.point_at(Point::new(150.0, 70.0));
    assert_eq!(ui.run(host_view(false), right_click()), vec![Host::Open]);
    // A right-click outside the open menu closes it; the target under the
    // barrier does not reopen it.
    ui.point_at(Point::new(900.0, 700.0));
    assert_eq!(ui.run(host_view(true), right_click()), vec![Host::Dismiss]);
    ui.point_at(Point::new(120.0, 60.0));
    assert_eq!(ui.run(host_view(true), right_click()), vec![Host::Dismiss]);
    ui.point_at(Point::new(900.0, 700.0));
    assert_eq!(
        ui.run(host_view(true), iced_test::simulator::click()),
        vec![Host::Dismiss]
    );
    // Escape.
    assert_eq!(
        ui.run(
            host_view(true),
            [iced_test::simulator::press_key(
                keyboard::Key::Named(Named::Escape),
                None
            )]
        ),
        vec![Host::Dismiss]
    );
    // A drag across the barrier is no tap: the menu stays.
    assert_eq!(
        ui.run(host_view(true), [press(), move_to(900.0, 640.0), release()]),
        vec![]
    );
    // With the menu closed, the target opens it again.
    ui.point_at(Point::new(150.0, 70.0));
    assert_eq!(ui.run(host_view(false), right_click()), vec![Host::Open]);
}

#[test]
fn a_dropdown_opens_under_its_trigger() {
    let mut ui = LiveUi::new(Size::new(1000.0, 800.0));
    let view = |open: bool| {
        let trigger = container(text("Interpretation")).width(120).height(30);
        let menu = open.then(|| {
            studio_menu(
                vec![StudioMenuItem::new("Tender", Host::Open)],
                STUDIO_MENU_WIDTH,
            )
        });
        container(
            menu_host(trigger, menu, Host::Dismiss).anchor(MenuAnchor::Below(MENU_ANCHOR_BELOW_DY)),
        )
        .padding(Padding {
            top: 100.0,
            left: 300.0,
            ..Padding::ZERO
        })
    };
    let tender = ui.find(view(true), "Tender").expect("open").bounds();
    // Below the trigger's bottom-left (300, 130), 4 px down, inside the
    // border and the row's 14 / 9 padding.
    assert!((tender.x - (300.0 + 1.0 + 14.0)).abs() < 0.5, "{tender:?}");
    assert!(tender.y >= 134.0 + 1.0 + 9.0 - 0.5, "{tender:?}");
    assert!(tender.y < 134.0 + 1.0 + 9.0 + 4.0, "{tender:?}");
}
