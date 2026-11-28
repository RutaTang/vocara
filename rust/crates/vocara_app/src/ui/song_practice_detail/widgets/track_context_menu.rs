//! Port of `ui/song_practice_detail/widgets/track_context_menu.dart`: the
//! dark studio menus — the track (lane) right-click menu with its inline
//! color picker, the FX bus menu, the clip menu, and the generic
//! [`studio_menu`] the in-app dropdowns use — the lane tints (the preset
//! palette, a take's stable default tint, the selected clip's border lift),
//! and the rename / delete-bus prompts.
//!
//! # How a menu opens
//!
//! Dart pushed an instant (no-animation) popup route at the pointer's global
//! position, dismissible by tapping outside, and handed each row a `close`
//! callback. Here a screen holds which menu is open: it builds the menu
//! ([`track_context_menu`], [`bus_context_menu`], [`clip_context_menu`],
//! [`studio_menu`]) and hands it to the [`menu_host`] wrapped around what was
//! clicked. The host remembers where the last press inside it landed (the
//! right-click, or the ⋯ button's tap-down — `details.globalPosition`) and
//! pops the menu there as an overlay over the whole window, clamped inside
//! it as `_MenuLayoutDelegate` did; [`MenuAnchor::Below`] anchors a dropdown
//! under its trigger instead (`menuAnchorBelow`). A position cannot travel in
//! the message: a press inside a scrolled lane list is seen in the list's
//! own coordinates, and only the host, which knows where it is drawn, can
//! turn it into a window position.
//!
//! Like the popup route, the open menu is modal with no barrier color: a tap
//! outside it (any button, released within the tap slop) or Escape reports
//! the host's `on_dismiss`, and while it is open nothing beneath sees the
//! pointer or the keyboard. Picking a row reports its action; the screen
//! closes the menu (`close()`), then runs the action. (Dart ran a color pick
//! before closing and every other row after; both happen in one update
//! here.)

use iced::advanced::layout::{self, Layout};
use iced::advanced::widget::{tree, Operation, Tree};
use iced::advanced::{overlay, renderer, Clipboard, Shell, Widget};
use iced::alignment::Vertical;
use iced::widget::{button, column, container, row, Column, Row, Space};
use iced::{
    keyboard, mouse, touch, Border, Color, Element, Event, Length, Padding, Pixels, Point,
    Rectangle, Renderer, Size, Theme, Vector,
};

use crate::ui::core::icons::{self, Icon};
use crate::ui::core::theme::{self, font, type_scale, Studio};
use crate::ui::core::widgets::cap_label::cap_label;
use crate::ui::core::widgets::confirm_dialog::{ConfirmDialog, StudioPrompt};
use crate::ui::core::widgets::fading_scrollbar::{no_scrollbar, scrollable};
use crate::ui::core::widgets::gesture::{any_tap_event, AnyTap};
use crate::ui::core::widgets::studio_text::studio_text;

// ------------------------------------------------------------------- tints

/// The preset lane tints (ARGB): `DESIGN.md`'s track colours, one muted
/// colour per track of equal lightness ([`Studio::TRACK_COLORS`]). The menu
/// offered Dart's sixteen saturated tints, which matched no default colour
/// and none of the design's.
pub const TRACK_COLOR_PALETTE: [i64; 5] = [
    0xFFD98C7E, // coral
    0xFFC9A46E, // ochre
    0xFF6FB39A, // sage
    0xFF8FA3D9, // periwinkle
    0xFFB48FD0, // lilac
];

/// Dart's `Color(value)`: an ARGB int (its low 32 bits) as a color.
pub fn argb(value: i64) -> Color {
    let v = value as u32;
    Color::from_rgba8(
        (v >> 16) as u8,
        (v >> 8) as u8,
        v as u8,
        f32::from((v >> 24) as u8) / 255.0,
    )
}

/// A brighter version of a lane's base color — used for the *border* of a
/// selected clip (a gentle brightness lift in the same hue, not pure white):
/// lightness +0.14, saturation +0.05, through Flutter's `HSLColor`.
pub fn selected_lane_color(base: Color) -> Color {
    let hsl = Hsl::from_color(base);
    Hsl {
        lightness: (hsl.lightness + 0.14).clamp(0.0, 1.0),
        saturation: (hsl.saturation + 0.05).clamp(0.0, 1.0),
        ..hsl
    }
    .to_color()
}

/// Flutter's `HSLColor`, on its 8-bit channels as Flutter reads a `Color`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Hsl {
    alpha: f64,
    hue: f64,
    saturation: f64,
    lightness: f64,
}

impl Hsl {
    /// `HSLColor.fromColor`.
    fn from_color(color: Color) -> Hsl {
        let [r, g, b, a] = color.into_rgba8();
        let (red, green, blue) = (
            f64::from(r) / 255.0,
            f64::from(g) / 255.0,
            f64::from(b) / 255.0,
        );
        let max = red.max(green.max(blue));
        let min = red.min(green.min(blue));
        let delta = max - min;
        // `_getHue`; Dart's `%` on doubles is never negative.
        let mut hue = if max == 0.0 {
            0.0
        } else if max == red {
            60.0 * ((green - blue) / delta).rem_euclid(6.0)
        } else if max == green {
            60.0 * (((blue - red) / delta) + 2.0)
        } else {
            60.0 * (((red - green) / delta) + 4.0)
        };
        // Hue is 0 when red == green == blue.
        if hue.is_nan() {
            hue = 0.0;
        }
        let lightness = (max + min) / 2.0;
        // Saturation can exceed 1.0 with rounding errors, so clamp it.
        let saturation = if min == max {
            0.0
        } else {
            (delta / (1.0 - (2.0 * lightness - 1.0).abs())).clamp(0.0, 1.0)
        };
        Hsl {
            alpha: f64::from(a) / 255.0,
            hue,
            saturation,
            lightness,
        }
    }

    /// `HSLColor.toColor` (`_colorFromHue`, then `Color.fromARGB`, which
    /// keeps each channel's low 8 bits).
    fn to_color(self) -> Color {
        let chroma = (1.0 - (2.0 * self.lightness - 1.0).abs()) * self.saturation;
        let secondary = chroma * (1.0 - ((self.hue / 60.0).rem_euclid(2.0) - 1.0).abs());
        let m = self.lightness - chroma / 2.0;
        let (red, green, blue) = match self.hue {
            h if h < 60.0 => (chroma, secondary, 0.0),
            h if h < 120.0 => (secondary, chroma, 0.0),
            h if h < 180.0 => (0.0, chroma, secondary),
            h if h < 240.0 => (0.0, secondary, chroma),
            h if h < 300.0 => (secondary, 0.0, chroma),
            _ => (chroma, 0.0, secondary),
        };
        let byte = |v: f64| ((v * 255.0).round() as i64 & 0xFF) as u8;
        Color::from_rgba8(
            byte(red + m),
            byte(green + m),
            byte(blue + m),
            f32::from(byte(self.alpha)) / 255.0,
        )
    }
}

// ------------------------------------------------------------------- menus

/// The track, bus and clip menus' width (`_kMenuWidth`).
pub const MENU_WIDTH: f32 = 268.0;

/// [`studio_menu`]'s default width (`showStudioMenu(width: 200)`).
pub const STUDIO_MENU_WIDTH: f32 = 200.0;

/// The menu's height cap: a long item list scrolls inside it instead of
/// running the full height of the viewport.
pub const MENU_MAX_HEIGHT: f32 = 440.0;

/// How close a menu may come to the window's edges.
pub const MENU_MARGIN: f32 = 8.0;

/// The track menu's flags (the non-callback half of `TrackMenuActions`).
/// Vocal-only actions are hidden for the accompaniment; `can_merge_down`
/// hides "Merge" when there is no lane below.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TrackMenu {
    pub is_vocal: bool,
    pub can_merge_down: bool,
    /// Whether to offer "Separate into backing + vocal" (only for a
    /// full-song backing/reference track).
    pub can_separate: bool,
    /// Whether to offer "Export take… / Export audio…" (the lane has audio).
    pub can_export: bool,
    /// When true the backing was already separated, so the action
    /// re-separates the kept original (different label).
    pub reseparate: bool,
    /// The track's current custom tint (ARGB), or `None` when it uses the
    /// default.
    pub current_color_value: Option<i64>,
    /// The colour the track shows with no tint of its own (the screen's
    /// `track_row::take_color`), for the automatic swatch.
    pub default_color: Color,
}

/// What the track menu's rows report (the callbacks of `TrackMenuActions`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackAction {
    /// Picks a lane tint (ARGB); `None` clears it, back to the default.
    PickColor(Option<i64>),
    Rename,
    Separate,
    /// Copies the lane's audio out to a user-chosen location.
    Export,
    AddNote,
    AddTechnique,
    /// Picks audio files and adds them as new clips on this take at the
    /// playhead — the click-to-upload twin of dropping audio onto the lane.
    AddClip,
    Duplicate,
    Split,
    MergeDown,
    Reanalyze,
    Delete,
}

/// `showTrackContextMenu`'s menu: the color grid, Rename, the backing's
/// extract, the vocal-take actions, export, re-analyze, and delete.
pub fn track_context_menu<'a>(menu: &TrackMenu) -> Element<'a, TrackAction> {
    let mut rows = column![
        color_grid(
            menu.current_color_value,
            menu.default_color,
            TrackAction::PickColor
        ),
        menu_divider(),
        menu_row(Some(Icon::Pencil), "Rename", TrackAction::Rename),
    ];
    if menu.can_separate {
        rows = rows.push(menu_row(
            Some(Icon::WandSparkles),
            if menu.reseparate {
                "Re-extract backing"
            } else {
                "Extract backing"
            },
            TrackAction::Separate,
        ));
    }
    // Notes and techniques annotate vocal takes only — the backing
    // (accompaniment/original) track has no use for them.
    if menu.is_vocal {
        rows = rows
            .push(menu_row(
                Some(Icon::StickyNote),
                "Add note at playhead",
                TrackAction::AddNote,
            ))
            .push(menu_row(
                Some(Icon::Waypoints),
                "Add technique at playhead",
                TrackAction::AddTechnique,
            ))
            .push(menu_row(
                Some(Icon::FilePlus),
                "Add audio clip at playhead…",
                TrackAction::AddClip,
            ))
            .push(menu_row(
                Some(Icon::Copy),
                "Duplicate",
                TrackAction::Duplicate,
            ))
            .push(menu_row(
                Some(Icon::Scissors),
                "Split at playhead",
                TrackAction::Split,
            ));
    }
    if menu.is_vocal && menu.can_merge_down {
        rows = rows.push(menu_row(
            Some(Icon::Combine),
            "Merge with take below",
            TrackAction::MergeDown,
        ));
    }
    // Exporting is the guarantee that audio is never trapped in the app's
    // private container — offered for any lane that has audio.
    if menu.can_export {
        rows = rows.push(menu_row(
            Some(Icon::Download),
            if menu.is_vocal {
                "Export take…"
            } else {
                "Export audio…"
            },
            TrackAction::Export,
        ));
    }
    rows = rows
        .push(menu_row(
            Some(Icon::RotateCw),
            "Re-analyze waveform",
            TrackAction::Reanalyze,
        ))
        .push(menu_divider())
        .push(
            MenuRow::new(
                if menu.is_vocal {
                    "Delete take"
                } else {
                    "Delete"
                },
                TrackAction::Delete,
            )
            .icon(Icon::Trash2)
            .danger(true),
        );
    menu_card(rows, MENU_WIDTH)
}

/// The FX-bus menu's flags — a slimmer cousin of [`TrackMenu`]: a bus has no
/// notes, clips, split/merge or re-analyze, so it offers just color, rename,
/// duplicate and delete.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BusMenu {
    pub current_color_value: Option<i64>,
    /// The colour the bus shows with no tint of its own
    /// (`track_row::bus_color`), for the automatic swatch.
    pub default_color: Color,
}

/// What the bus menu's rows report (`BusMenuActions`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BusAction {
    /// Picks the bus's tint (ARGB); `None` clears it, back to the default.
    PickColor(Option<i64>),
    Rename,
    Duplicate,
    Delete,
}

/// `showBusContextMenu`'s menu, styled like the track menu.
pub fn bus_context_menu<'a>(menu: &BusMenu) -> Element<'a, BusAction> {
    let rows = column![
        color_grid(
            menu.current_color_value,
            menu.default_color,
            BusAction::PickColor
        ),
        menu_divider(),
        menu_row(Some(Icon::Pencil), "Rename", BusAction::Rename),
        menu_row(Some(Icon::Copy), "Duplicate", BusAction::Duplicate),
        menu_divider(),
        MenuRow::new("Delete", BusAction::Delete)
            .icon(Icon::Trash2)
            .danger(true),
    ];
    menu_card(rows, MENU_WIDTH)
}

/// `confirmDeleteBus`: asks before deleting the FX bus named `bus_name`.
/// Every surface that offers the delete (the timeline lane's menu, the mixer
/// strip's menu and trash icon) goes through this one prompt: a delete takes
/// the bus's FX chain and every send into it with it, and there is no undo.
pub fn confirm_delete_bus(bus_name: &str) -> ConfirmDialog {
    ConfirmDialog::new(format!("Delete \"{bus_name}\"?"))
        .body("Removing this FX bus re-routes anything feeding it to the master.")
}

/// What the clip menu's rows report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipAction {
    Export,
    Delete,
}

/// `showClipContextMenu`'s menu for a single clip: export (the per-clip
/// escape hatch for multi-clip lanes), when the screen wires it, and delete.
pub fn clip_context_menu<'a>(can_export: bool) -> Element<'a, ClipAction> {
    let mut rows = Column::new();
    if can_export {
        rows = rows.push(menu_row(
            Some(Icon::Download),
            "Export clip…",
            ClipAction::Export,
        ));
    }
    rows = rows.push(
        MenuRow::new("Delete clip", ClipAction::Delete)
            .icon(Icon::Trash2)
            .danger(true),
    );
    menu_card(rows, MENU_WIDTH)
}

/// One entry in a [`studio_menu`] popup.
#[derive(Debug, Clone)]
pub struct StudioMenuItem<M> {
    pub label: String,
    pub on_tap: M,
    pub icon: Option<Icon>,
    /// Optional one-line description shown faintly under the label.
    pub subtitle: Option<String>,
    pub selected: bool,
    pub danger: bool,
    /// Renders a divider above this item.
    pub divider: bool,
}

impl<M> StudioMenuItem<M> {
    pub fn new(label: impl Into<String>, on_tap: M) -> Self {
        Self {
            label: label.into(),
            on_tap,
            icon: None,
            subtitle: None,
            selected: false,
            danger: false,
            divider: false,
        }
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn danger(mut self, danger: bool) -> Self {
        self.danger = danger;
        self
    }

    pub fn divider(mut self, divider: bool) -> Self {
        self.divider = divider;
        self
    }
}

/// `showStudioMenu`: an instant dark studio menu of `items`, `width` wide
/// ([`STUDIO_MENU_WIDTH`] by default) — used by the in-app dropdowns so
/// every menu opens the same way.
pub fn studio_menu<'a, M: Clone + 'a>(items: Vec<StudioMenuItem<M>>, width: f32) -> Element<'a, M> {
    let mut rows = Column::new();
    for it in items {
        if it.divider {
            rows = rows.push(menu_divider());
        }
        let mut entry = MenuRow::new(it.label, it.on_tap)
            .danger(it.danger)
            .selected(it.selected);
        entry.icon = it.icon;
        entry.subtitle = it.subtitle;
        rows = rows.push(entry);
    }
    menu_card(rows, width)
}

/// `showRenameDialog`: the modal text prompt used to name/rename something —
/// "Rename track" / "Track name" / "Rename", prefilled with `current`.
/// Callers retitle it through [`StudioPrompt`]'s fields and builders (the
/// bus's "Rename bus" / "Bus name", the interpretations' prompts).
/// Confirm/cancel prompts use the shared [`ConfirmDialog`].
pub fn rename_dialog(current: &str) -> StudioPrompt {
    StudioPrompt::new("Rename track")
        .hint("Track name")
        .initial(current)
        .confirm_label("Rename")
}

/// The space between a menu card's edge and its rows.
pub const MENU_PADDING: f32 = 4.0;

/// A plain row's height (a row with a subtitle grows).
pub const MENU_ROW_HEIGHT: f32 = 26.0;

/// A row's inset, its icon box and the gap after the icon.
const ROW_PADDING_X: f32 = 8.0;
pub const MENU_ICON: f32 = 14.0;
const ICON_GAP: f32 = 8.0;

/// The menu's card (`DESIGN.md`, Menus: `theme::menu`, a `SURFACE` card,
/// radius 8, an inset `BORDER_STRONG` outline, a soft shadow), its rows
/// [`MENU_PADDING`] in; the rows scroll (no scrollbar) inside a
/// [`MENU_MAX_HEIGHT`] cap.
fn menu_card<'a, M: 'a>(rows: Column<'a, M>, width: f32) -> Element<'a, M> {
    container(
        container(no_scrollbar(scrollable(rows.width(Length::Fill)))).max_height(MENU_MAX_HEIGHT),
    )
    .width(width)
    .padding(MENU_PADDING)
    .style(theme::menu)
    .clip(true)
    .into()
}

/// `_MenuDivider`: a 1 px `BORDER` separator, 4 px clear of the rows each
/// side and as wide as the rows' hover.
fn menu_divider<'a, M: 'a>() -> Element<'a, M> {
    container(
        container(Space::new())
            .width(Length::Fill)
            .height(1)
            .style(theme::fill(Studio::BORDER)),
    )
    .width(Length::Fill)
    .height(9)
    .align_y(Vertical::Center)
    .into()
}

/// A plain row: `icon` (if any), `label`.
fn menu_row<'a, M: Clone + 'a>(icon: Option<Icon>, label: &str, on_tap: M) -> Element<'a, M> {
    let mut entry = MenuRow::new(label, on_tap);
    entry.icon = icon;
    entry.into()
}

/// `_MenuRow` (`DESIGN.md`, Menus): a 26 px row (`HOVER` under the pointer,
/// radius 4), 8 px in: a 14 px `TEXT_SECONDARY` icon 8 px before the 12 px
/// label (one line), an 11 px `TEXT_FAINT` subtitle under it (two lines at
/// most), and an `ACCENT_BRIGHT` check at its end when selected. A danger
/// row's label and icon are `RECORD_TEXT`.
pub struct MenuRow<M> {
    icon: Option<Icon>,
    label: String,
    on_tap: M,
    danger: bool,
    selected: bool,
    subtitle: Option<String>,
}

impl<M> MenuRow<M> {
    pub fn new(label: impl Into<String>, on_tap: M) -> Self {
        Self {
            icon: None,
            label: label.into(),
            on_tap,
            danger: false,
            selected: false,
            subtitle: None,
        }
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn danger(mut self, danger: bool) -> Self {
        self.danger = danger;
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    /// The row's label color.
    pub fn color(&self) -> Color {
        if self.danger {
            Studio::RECORD_TEXT
        } else {
            Studio::TEXT_PRIMARY
        }
    }

    /// The row's icon color.
    pub fn icon_color(&self) -> Color {
        if self.danger {
            Studio::RECORD_TEXT
        } else {
            Studio::TEXT_SECONDARY
        }
    }

    /// The label's weight: medium on the current item.
    pub fn font(&self) -> iced::Font {
        if self.selected {
            font::MEDIUM
        } else {
            font::REGULAR
        }
    }
}

/// The check at the end of a menu's current item.
pub const SELECTED_CHECK: Color = Studio::ACCENT_BRIGHT;

impl<'a, M: Clone + 'a> From<MenuRow<M>> for Element<'a, M> {
    fn from(r: MenuRow<M>) -> Self {
        let color = r.color();
        let weight = r.font();
        let mut content = row![].spacing(ICON_GAP).align_y(Vertical::Center);
        if let Some(icon) = r.icon {
            content = content.push(
                container(icons::icon(icon, MENU_ICON, r.icon_color()))
                    .center_x(MENU_ICON)
                    .center_y(MENU_ICON),
            );
        }
        // One line, ellipsized.
        let mut labels = column![studio_text(r.label)
            .size(type_scale::BODY)
            .line_height(Pixels(type_scale::LINE))
            .font(weight)
            .color(color)
            .ellipsis(1)]
        .width(Length::Fill);
        let two_lines = r.subtitle.is_some();
        if let Some(subtitle) = r.subtitle {
            labels = labels.push(
                // Two lines at most, the second ellipsized.
                studio_text(subtitle)
                    .size(type_scale::LABEL)
                    .line_height(Pixels(14.0))
                    .color(Studio::TEXT_FAINT)
                    .ellipsis(2),
            );
        }
        content = content.push(labels);
        if r.selected {
            content = content.push(icons::icon(Icon::Check, MENU_ICON, SELECTED_CHECK));
        }
        let row = button(container(content).center_y(Length::Fill))
            .width(Length::Fill)
            .height(if two_lines {
                Length::Shrink
            } else {
                Length::Fixed(MENU_ROW_HEIGHT)
            })
            .padding(Padding::from([
                if two_lines { 5.0 } else { 0.0 },
                ROW_PADDING_X,
            ]))
            .style(menu_row_style)
            .on_press(r.on_tap);
        row.into()
    }
}

/// A menu row's face: `HOVER` under the pointer at once, radius 4.
fn menu_row_style(_: &Theme, status: button::Status) -> button::Style {
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style {
        background: hovered.then_some(Studio::HOVER.into()),
        text_color: Studio::TEXT_PRIMARY,
        border: Border {
            radius: theme::radius::SMALL.into(),
            ..Border::default()
        },
        shadow: iced::Shadow::default(),
        snap: true,
    }
}

/// The color grid's swatches: their side, the gap between them, and how
/// many a row holds.
pub const SWATCH: f32 = 20.0;
pub const SWATCH_GAP: f32 = 10.0;
const SWATCHES_A_ROW: usize = 8;

/// The color grid's inset inside the menu's padding.
pub const COLOR_GRID_PADDING: Padding = Padding {
    top: 6.0,
    right: ROW_PADDING_X,
    bottom: 8.0,
    left: ROW_PADDING_X,
};

/// The automatic swatch: a [`SWATCH`] px tall pill this wide, reading
/// [`AUTO`] on the lane's default colour.
pub const AUTO_SWATCH_WIDTH: f32 = 40.0;
pub const AUTO: &str = "Auto";

/// One swatch of the colour grid: its colour, what a pick stores (`None`
/// clears the lane's tint, back to its default colour), and whether it is
/// the lane's colour now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Swatch {
    pub color: Color,
    pub value: Option<i64>,
    pub selected: bool,
}

/// The colour grid's swatches, in order: the automatic one, in the lane's
/// `default` colour; the palette's; and a stored tint the palette does not
/// hold (an earlier palette's), so the lane's colour is always on the grid.
/// The one the lane shows is ringed: its stored tint's, else the automatic
/// one. (No swatch was ringed for a lane in its default colour, and none
/// could restore it.)
pub fn swatches(current: Option<i64>, default: Color) -> Vec<Swatch> {
    let mut out = vec![Swatch {
        color: default,
        value: None,
        selected: current.is_none(),
    }];
    out.extend(TRACK_COLOR_PALETTE.iter().map(|&value| Swatch {
        color: argb(value),
        value: Some(value),
        selected: current == Some(value),
    }));
    if let Some(value) = current.filter(|v| !TRACK_COLOR_PALETTE.contains(v)) {
        out.push(Swatch {
            color: argb(value),
            value: Some(value),
            selected: true,
        });
    }
    out
}

/// `_ColorGrid`: the inline lane-color picker — a "Color" section label
/// (11 px semibold `TEXT_SECONDARY`) over the [`swatches`], eight to a row.
/// `default` is the colour the lane shows with no tint of its own.
fn color_grid<'a, M: Clone + 'a>(
    current: Option<i64>,
    default: Color,
    on_pick: impl Fn(Option<i64>) -> M,
) -> Element<'a, M> {
    let all = swatches(current, default);
    let rows = all.chunks(SWATCHES_A_ROW).map(|chunk| {
        Row::with_children(chunk.iter().map(|s| color_swatch(*s, on_pick(s.value))))
            .spacing(SWATCH_GAP)
            .into()
    });
    container(column![
        container(cap_label("Color")).padding(Padding {
            bottom: 8.0,
            ..Padding::ZERO
        }),
        Column::with_children(rows).spacing(SWATCH_GAP),
    ])
    .padding(COLOR_GRID_PADDING)
    .into()
}

/// A swatch's outline: 2 px `TEXT_PRIMARY` round the lane's colour, 2 px of
/// it at half strength under the pointer, else 1 px of faint white so a dark
/// tint keeps its edge on the card.
pub fn swatch_outline(selected: bool, hovered: bool) -> (Color, f32) {
    if selected {
        (Studio::TEXT_PRIMARY, 2.0)
    } else if hovered {
        (theme::alpha(Studio::TEXT_PRIMARY, 0.5), 2.0)
    } else {
        (theme::alpha(Color::WHITE, 0.10), 1.0)
    }
}

/// `_ColorSwatch`: a [`SWATCH`] px disc of the swatch's colour (the
/// automatic one a pill reading [`AUTO`]), outlined by [`swatch_outline`].
fn color_swatch<'a, M: Clone + 'a>(swatch: Swatch, on_tap: M) -> Element<'a, M> {
    let Swatch {
        color, selected, ..
    } = swatch;
    let face: Element<'a, M> = if swatch.value.is_none() {
        container(
            studio_text(AUTO)
                .size(type_scale::AXIS)
                .font(font::SEMIBOLD)
                .color(Studio::BACKGROUND),
        )
        .center_x(AUTO_SWATCH_WIDTH)
        .center_y(SWATCH)
        .into()
    } else {
        Space::new().width(SWATCH).height(SWATCH).into()
    };
    button(face)
        .padding(0)
        .style(move |_, status| {
            let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
            let (outline, width) = swatch_outline(selected, hovered);
            button::Style {
                background: Some(color.into()),
                text_color: Studio::BACKGROUND,
                border: Border {
                    color: outline,
                    width,
                    radius: (SWATCH / 2.0).into(),
                },
                shadow: iced::Shadow::default(),
                snap: true,
            }
        })
        .on_press(on_tap)
        .into()
}

// -------------------------------------------------------------- the host

/// Where a [`MenuHost`] pops its menu.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum MenuAnchor {
    /// At the last press inside the host: the right-click or tap-down that
    /// opened it (`details.globalPosition`).
    #[default]
    Press,
    /// Just below the host's bottom-left corner, `dy` lower
    /// (`menuAnchorBelow(context, dy: 4)`): a dropdown under its trigger.
    Below(f32),
}

/// [`MenuAnchor::Below`]'s default gap.
pub const MENU_ANCHOR_BELOW_DY: f32 = 4.0;

/// `content`, with `menu` popped over the window while `Some` (see the
/// module docs).
pub struct MenuHost<'a, M> {
    content: Element<'a, M>,
    menu: Option<Element<'a, M>>,
    on_dismiss: M,
    anchor: MenuAnchor,
}

/// Wraps `content`; while `menu` is `Some` it is open at the last press
/// inside `content`, and a tap outside it or Escape reports `on_dismiss`.
pub fn menu_host<'a, M>(
    content: impl Into<Element<'a, M>>,
    menu: Option<Element<'a, M>>,
    on_dismiss: M,
) -> MenuHost<'a, M> {
    MenuHost {
        content: content.into(),
        menu,
        on_dismiss,
        anchor: MenuAnchor::Press,
    }
}

impl<M> MenuHost<'_, M> {
    pub fn anchor(mut self, anchor: MenuAnchor) -> Self {
        self.anchor = anchor;
        self
    }
}

/// Where `_MenuLayoutDelegate` puts a menu of `menu` size opened at `at` in
/// a window of `window` size: at the point, pulled back inside the
/// [`MENU_MARGIN`] when it would run past the right or bottom edge, and held
/// at the margin. (Dart's `clamp(8, size.width)` threw in a window narrower
/// than the margin; held at the margin here.)
pub fn menu_position(at: Point, menu: Size, window: Size) -> Point {
    let mut x = at.x;
    let mut y = at.y;
    if x + menu.width > window.width - MENU_MARGIN {
        x = window.width - menu.width - MENU_MARGIN;
    }
    if y + menu.height > window.height - MENU_MARGIN {
        y = window.height - menu.height - MENU_MARGIN;
    }
    Point::new(
        x.max(MENU_MARGIN).min(window.width.max(MENU_MARGIN)),
        y.max(MENU_MARGIN).min(window.height.max(MENU_MARGIN)),
    )
}

/// The host's memory: where the last press inside it landed, and a tap on
/// the barrier in flight.
#[derive(Debug, Default)]
struct HostState {
    /// The last press inside the host, from its top-left.
    press: Option<Vector>,
    /// A tap on the barrier (outside the open menu) in flight: its release
    /// within the tap slop dismisses (Flutter's barrier dismisses on
    /// a tap's up, of any button).
    barrier: AnyTap,
}

impl<'a, M: Clone + 'a> Widget<M, Theme, Renderer> for MenuHost<'a, M> {
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.content.as_widget().size_hint()
    }

    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<HostState>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(HostState::default())
    }

    fn children(&self) -> Vec<Tree> {
        vec![
            Tree::new(&self.content),
            self.menu.as_ref().map_or_else(Tree::empty, Tree::new),
        ]
    }

    fn diff(&self, tree: &mut Tree) {
        tree.children.resize_with(2, Tree::empty);
        tree.children[0].diff(&self.content);
        match &self.menu {
            Some(menu) => tree.children[1].diff(menu),
            None => tree.children[1] = Tree::empty(),
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
        let state = tree.state.downcast_mut::<HostState>();
        if self.menu.is_none() {
            state.barrier = AnyTap::default();
        }
        // Every press inside counts, the ones a child took too (the ⋯
        // button's tap-down): the last one opened the menu.
        let bounds = layout.bounds();
        let pressed = match event {
            Event::Mouse(mouse::Event::ButtonPressed(_)) => cursor.position_over(bounds),
            Event::Touch(touch::Event::FingerPressed { position, .. }) => {
                bounds.contains(*position).then_some(*position)
            }
            _ => None,
        };
        if let Some(p) = pressed {
            state.press = Some(p - bounds.position());
        }
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
        let bounds = layout.bounds();
        let Tree {
            state, children, ..
        } = tree;
        let [content_tree, menu_tree] = children.as_mut_slice() else {
            return None;
        };
        match &mut self.menu {
            Some(menu) => {
                let host = state.downcast_mut::<HostState>();
                // The host as drawn on the window (a scrolled list moves it).
                let origin = bounds.position() + translation;
                let at = match self.anchor {
                    MenuAnchor::Press => origin + host.press.unwrap_or(Vector::ZERO),
                    MenuAnchor::Below(dy) => origin + Vector::new(0.0, bounds.height + dy),
                };
                Some(overlay::Element::new(Box::new(OpenMenu {
                    menu,
                    tree: menu_tree,
                    at,
                    barrier: &mut host.barrier,
                    on_dismiss: self.on_dismiss.clone(),
                })))
            }
            None => self.content.as_widget_mut().overlay(
                content_tree,
                layout,
                renderer,
                viewport,
                translation,
            ),
        }
    }
}

impl<'a, M: Clone + 'a> From<MenuHost<'a, M>> for Element<'a, M> {
    fn from(host: MenuHost<'a, M>) -> Self {
        Element::new(host)
    }
}

/// The open menu over the whole window (`_InstantMenuRoute`).
struct OpenMenu<'a, 'b, M> {
    menu: &'b mut Element<'a, M>,
    tree: &'b mut Tree,
    at: Point,
    barrier: &'b mut AnyTap,
    on_dismiss: M,
}

impl<M: Clone> overlay::Overlay<M, Theme, Renderer> for OpenMenu<'_, '_, M> {
    fn layout(&mut self, renderer: &Renderer, bounds: Size) -> layout::Node {
        // Laid out within the window less the margin all round
        // (`BoxConstraints.loose(biggest).deflate(8)`).
        let limits = layout::Limits::new(
            Size::ZERO,
            Size::new(
                (bounds.width - 2.0 * MENU_MARGIN).max(0.0),
                (bounds.height - 2.0 * MENU_MARGIN).max(0.0),
            ),
        );
        let node = self
            .menu
            .as_widget_mut()
            .layout(self.tree, renderer, &limits);
        let at = menu_position(self.at, node.size(), bounds);
        // The node spans the window (the barrier); its one child is the menu.
        layout::Node::with_children(bounds, vec![node.move_to(at)])
    }

    fn draw(
        &self,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
    ) {
        let Some(menu) = layout.children().next() else {
            return;
        };
        self.menu.as_widget().draw(
            self.tree,
            renderer,
            theme,
            style,
            menu,
            cursor,
            &layout.bounds(),
        );
    }

    fn operate(&mut self, layout: Layout<'_>, renderer: &Renderer, operation: &mut dyn Operation) {
        if let Some(menu) = layout.children().next() {
            self.menu
                .as_widget_mut()
                .operate(self.tree, menu, renderer, operation);
        }
    }

    fn update(
        &mut self,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
    ) {
        let Some(menu) = layout.children().next() else {
            return;
        };
        let outside = |p: Point| !menu.bounds().contains(p);
        match event {
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(keyboard::key::Named::Escape),
                ..
            }) => {
                shell.publish(self.on_dismiss.clone());
                shell.capture_event();
                return;
            }
            _ => {
                // The barrier: a tap outside the menu, of any button, on its
                // release within the tap slop.
                let (tapped, taken) =
                    any_tap_event(self.barrier, event, outside, cursor.position());
                if tapped {
                    shell.publish(self.on_dismiss.clone());
                }
                if taken {
                    shell.capture_event();
                    return;
                }
            }
        }
        self.menu.as_widget_mut().update(
            self.tree,
            event,
            menu,
            cursor,
            renderer,
            clipboard,
            shell,
            &layout.bounds(),
        );
        // The barrier: nothing beneath sees the pointer or the keyboard.
        if matches!(
            event,
            Event::Mouse(_) | Event::Touch(_) | Event::Keyboard(_) | Event::InputMethod(_)
        ) {
            shell.capture_event();
        }
    }

    fn mouse_interaction(
        &self,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        let interaction =
            layout
                .children()
                .next()
                .map_or_else(mouse::Interaction::default, |menu| {
                    self.menu.as_widget().mouse_interaction(
                        self.tree,
                        menu,
                        cursor,
                        &layout.bounds(),
                        renderer,
                    )
                });
        // Anything but `None` keeps the cursor from the page beneath: the
        // barrier covers the whole window.
        if interaction == mouse::Interaction::None {
            mouse::Interaction::Idle
        } else {
            interaction
        }
    }
}

#[cfg(test)]
mod tests;
