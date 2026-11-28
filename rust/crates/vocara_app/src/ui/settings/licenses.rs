//! Port of `ui/settings/licenses.dart` and of the license page Settings
//! opened with it (Flutter's stock `LicensePage` under the window chrome,
//! nudged toward the studio palette by `settings_screen.dart`'s
//! `_LicensesPage`).
//!
//! # Where the licenses come from
//!
//! Flutter collected every Dart package's LICENSE file into its
//! `LicenseRegistry` automatically, and `licenses.dart` added hand-maintained
//! entries for what ships in the bundle but is not a pub package (the
//! media_kit frameworks, SQLite, the engine's Rust crates). The Rust app is
//! one statically linked binary, so its third-party code is the crates it
//! links, plus what those crates compile in or the app embeds:
//!
//! - **Crates**: `assets/licenses/crates.json`, generated from
//!   `cargo metadata` for the shipped build (`vocara_app` with `device`, for
//!   both macOS targets) and committed. It lists every crate reachable
//!   through normal dependencies, with its license expression and the
//!   license files it packages (stored once each). A test regenerates it and
//!   fails when the committed copy is stale (`licenses/generate.rs` says how
//!   to refresh it). Committing the list keeps the build free of a
//!   `cargo metadata` call (a build script running cargo can deadlock on
//!   cargo's own locks) and makes every license change reviewable.
//! - **Bundled components** that no crate's license covers: the Inter
//!   typeface the app embeds (its OFL text ships with the font), SQLite,
//!   which rusqlite compiles in, Material Icons, whose
//!   `arrow_back_ios_new` outline this page's back button draws and whose
//!   `volume_*` and `fullscreen*` outlines the technique video's controls
//!   draw (its Apache License text is
//!   `assets/licenses/material-icons/LICENSE`), and Flutter, whose
//!   `AnimatedIcons.play_pause` keyframes those controls animate (its BSD
//!   license text is `assets/licenses/flutter/LICENSE`), and CHIMERA, a
//!   voice synthesizer whose anatomy, airway acoustics and head drawing
//!   the `vocara_tract` crate ports (a workspace crate, so not in the
//!   generated list: its MIT notice is `vocara_tract::CHIMERA_LICENSE`).
//!   Keep this
//!   list in sync when the app starts shipping another non-crate component
//!   (a media framework for the technique videos, say). The engine's own
//!   code is first-party, covered by the app's copyright; its crates are in
//!   the generated list.
//!
//! [`licenses`] gathers both once (registration is idempotent, as
//! `registerBundledComponentLicenses` was), as Flutter `LicenseEntry`s:
//! packages and a text that [`license_paragraphs`] splits as
//! `LicenseEntryWithLineBreaks` did.
//!
//! # The page
//!
//! [`LicensesPage`] is `LicensePage`'s lateral (master/detail) layout, which
//! Flutter used from 840 px wide — always here, the window being at least
//! 960 wide: an app bar, the about header and the package list on the left,
//! and the selected package's licenses on a card to the right. Colours are
//! the theme Settings gave it (`Studio.background` scaffold,
//! `Studio.surface` cards and app bar) over forui's approximate Material
//! theme (zinc dark), on macOS:
//!
//! - **Loading.** The registry is gathered off the UI thread
//!   ([`LicensesPage::load`]), as the page's `FutureBuilder` waited on
//!   `LicenseRegistry.licenses`: meanwhile the master pane is the about
//!   block over Material's spinner on a card with no elevation (an error
//!   shows centred instead). Once gathered, the list shows and the first
//!   package's detail comes in like any other (the switcher started on an
//!   empty child; `_initDefaultDetailPage`'s post-frame callback re-keys
//!   it).
//! - **The app bar**: the page's unified toolbar (`widgets::toolbar`,
//!   `DESIGN.md`: the back button and "Licenses" after it, past the
//!   traffic lights), over the master/detail scaffold's empty 56 px action
//!   row. Once the package list scrolls under it (its
//!   `ScrollNotificationObserver`), that row takes the scheme's surface
//!   tint (8 % of `primary`) at once.
//! - **The package rows** are two-line `ListTile`s at the desktop's compact
//!   density (`licenses/list_tile.rs`): 64 tall, the title's baseline 32
//!   down, the subtitle's 48; the selected one on the theme's
//!   `highlightColor`. Their `InkWell` lights them in forui's rounded
//!   list-tile shape (radius 8), over the selection too: the theme's
//!   `hoverColor` under the pointer (fading over 50 ms), its
//!   `highlightColor` while pressed (over 200 ms).
//! - **The detail** sits on a `DraggableScrollableSheet`
//!   (`licenses/sheet.rs`): its card starts right under the toolbar (the
//!   sheet's size is a share of the page's height, the page the whole
//!   window), and a trackpad's two-finger drag grows it
//!   to the page's full height before the list scrolls (shrinking it back
//!   after the list is at its top), its flick flinging the sheet and then
//!   the list as Flutter's simulation did, where a mouse wheel only
//!   scrolls the list.
//!   The header (the pinned `SliverAppBar`) centres the package over its
//!   license count, no gap between, and takes the surface tint once the
//!   list scrolls under it; each license follows a `Divider` in forui's
//!   `dividerTheme` color (`secondary`).
//! - **The switch.** A newly selected package's licenses come in as the
//!   detail's `AnimatedSwitcher` brought them, over 500 ms: up from a
//!   quarter of the page's height (the transition slides the switcher's
//!   whole child, `Curves.fastOutSlowIn`) as they fade in
//!   (`Curves.easeIn`), the last package's going the same way back over
//!   them (`FadeUpwardsPageTransition`), keeping its scroll and its sheet
//!   (the switcher's children are keyed); the one going takes no input. A
//!   package's licenses fill in as `_PackageLicensePage` filled them, a
//!   license at a time (Flutter parsed each in a task between frames, so a
//!   few could land per frame; here one lands per frame), under Material's
//!   spinner until the last has. "Powered by Flutter" reads "Powered by
//!   iced".
//!
//! The look is the design system's (`DESIGN.md`), on Flutter's layout:
//! the master and the detail card are `Studio::SURFACE` (the card an inset
//! `BORDER` outline, radius 8 at its top), the package rows a 13 px name
//! over its 11 px `TEXT_FAINT` count, the about block a 15 px semibold
//! name over 12 and 11 px lines, the detail's header a 13 px semibold
//! package over its count, the licenses 12 px `TEXT_SECONDARY` on 16 px
//! lines under `BORDER` dividers, the spinners neutral.
//!
//! Where it still differs:
//!
//! - Flutter's scrollbar on the detail spanned the whole scroll view, the
//!   pinned header's 56 px included; here the list under the header is
//!   the scrollable, so the bar starts under the header.
//! - The registry is gathered once per run, not on each opening: a later
//!   opening shows the spinner only until the gathering task reports.
//! - The outgoing detail takes no input at all; Flutter's could still be
//!   reached where the incoming one did not cover it.
//! - A title too wide to centre (unreachable on a 960 px window) would be
//!   pushed to the start and ellipsized in Flutter; here it is not.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::ui::core::text;
use iced::alignment::Horizontal;
use iced::border::Radius;
use iced::widget::text::{LineHeight, Wrapping};
use iced::widget::{button, column, container, row, stack, Column, Space};
use iced::{
    Background, Border, Color, Element, Length, Padding, Pixels, Shadow, Task, Theme, Vector,
};
use serde::Deserialize;
use vocara_services::update_check_service::UpdateCheckService;

use crate::ui::core::motion::{Controller, Curve};
use crate::ui::core::theme::{self, font, type_scale, Studio, Zinc};
use crate::ui::core::widgets::animated::ink_hover;
use crate::ui::core::widgets::fading_scrollbar::{scrollable, Fading};
use crate::ui::core::widgets::opacity_layer::opacity_layer;
use crate::ui::core::widgets::slide::slide;
use crate::ui::core::widgets::studio_hint::spinner;
use crate::ui::core::widgets::toolbar;

pub mod list_tile;
pub mod sheet;

use list_tile::two_line_tile;
use sheet::{keyed_stack, scroll_watch, sheet, SCROLLED_UNDER_TINT};

#[cfg(test)]
mod generate;
#[cfg(test)]
mod page_tests;

// ------------------------------------------------------------------ entries

/// A license and the packages it covers (`LicenseEntryWithLineBreaks`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LicenseEntry {
    pub packages: Vec<String>,
    pub text: String,
}

impl LicenseEntry {
    /// The text split into paragraphs (see [`license_paragraphs`]).
    pub fn paragraphs(&self) -> Vec<LicenseParagraph> {
        license_paragraphs(&self.text)
    }
}

/// A paragraph of a license (`LicenseParagraph`): its text and its
/// indentation level, or [`LicenseParagraph::CENTERED_INDENT`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LicenseParagraph {
    pub text: String,
    pub indent: i32,
}

impl LicenseParagraph {
    /// A centred heading.
    pub const CENTERED_INDENT: i32 = -1;
}

/// `LicenseEntryWithLineBreaks.paragraphs`: splits a license's text into
/// paragraphs.
///
/// - Lines starting with a different number of space characters than the
///   previous line start a new paragraph, with those spaces removed.
/// - Blank lines start a new paragraph.
/// - Other line breaks are replaced by a single space character.
/// - Leading spaces on a line are removed.
///
/// For each paragraph, a rough heuristic guesses how indented it is (a third
/// of its first line's indentation, a tab counting 8), or whether it is
/// centred (indented more than 10).
///
/// Unlike Flutter's, a list item's hanging indent continues its paragraph:
/// a line indented to where the text after the first line's marker starts
/// ("2. ", "(a) ", "* ") goes on with the item, unless it starts an item of
/// its own, and the next item after it starts a paragraph whatever its
/// indent. Flutter broke the Apache License's numbered sections and the
/// BSD licenses' bullets after their first line, mid-sentence ("Subject to
/// the terms and conditions of" / "this License, ..."), and ran a bullet
/// on into the next.
pub fn license_paragraphs(text: &str) -> Vec<LicenseParagraph> {
    let chars: Vec<char> = text.chars().collect();
    let mut line_start = 0usize;
    let mut position = 0usize;
    let mut last_line_indent = 0i32;
    let mut current_line_indent = 0i32;
    let mut paragraph_indentation: Option<i32> = None;
    // Where a list item's text after its marker starts: its lines' hanging
    // indent.
    let mut hang: Option<i32> = None;
    let mut in_paragraph = false;
    let mut lines: Vec<String> = Vec::new();
    let mut result = Vec::new();

    let take_paragraph = |lines: &mut Vec<String>, indent: Option<i32>| LicenseParagraph {
        text: std::mem::take(lines).join(" "),
        indent: indent.unwrap_or(0),
    };

    while position < chars.len() {
        let c = chars[position];
        if !in_paragraph {
            match c {
                ' ' => {
                    line_start = position + 1;
                    current_line_indent += 1;
                }
                '\t' => {
                    line_start = position + 1;
                    current_line_indent += 8;
                }
                '\r' | '\n' | '\u{c}' => {
                    if !lines.is_empty() {
                        result.push(take_paragraph(&mut lines, paragraph_indentation));
                    }
                    if c == '\r' && position + 1 < chars.len() && chars[position + 1] == '\n' {
                        position += 1;
                    }
                    last_line_indent = 0;
                    current_line_indent = 0;
                    paragraph_indentation = None;
                    hang = None;
                    line_start = position + 1;
                }
                _ => {
                    // This is a bit of a hack for the LGPL 2.1, which does
                    // something like this:
                    //
                    //   [this is a
                    //    single paragraph]
                    //
                    // ...near the top.
                    if c == '[' {
                        current_line_indent += 1;
                    }
                    let marker = list_marker(&chars[position..]);
                    let hanging =
                        lines.len() == 1 && hang == Some(current_line_indent) && marker.is_none();
                    // The next item of a list starts a paragraph of its own,
                    // at whatever indent.
                    let next_item = hang.is_some() && marker.is_some();
                    if !lines.is_empty()
                        && ((current_line_indent > last_line_indent && !hanging) || next_item)
                    {
                        result.push(take_paragraph(&mut lines, paragraph_indentation));
                        paragraph_indentation = None;
                    }
                    if lines.is_empty() {
                        hang = marker.map(|width| current_line_indent + width);
                    }
                    // The following is a wild heuristic for guessing the
                    // indentation level. It happens to work for common
                    // variants of the BSD and LGPL licenses.
                    if paragraph_indentation.is_none() {
                        paragraph_indentation = Some(if current_line_indent > 10 {
                            LicenseParagraph::CENTERED_INDENT
                        } else {
                            current_line_indent / 3
                        });
                    }
                    in_paragraph = true;
                }
            }
        } else {
            match c {
                '\n' => {
                    lines.push(chars[line_start..position].iter().collect());
                    last_line_indent = current_line_indent;
                    current_line_indent = 0;
                    line_start = position + 1;
                    in_paragraph = false;
                }
                '\u{c}' => {
                    lines.push(chars[line_start..position].iter().collect());
                    result.push(take_paragraph(&mut lines, paragraph_indentation));
                    last_line_indent = 0;
                    current_line_indent = 0;
                    paragraph_indentation = None;
                    hang = None;
                    line_start = position + 1;
                    in_paragraph = false;
                }
                _ => {}
            }
        }
        position += 1;
    }
    if in_paragraph {
        lines.push(chars[line_start..position].iter().collect());
        result.push(take_paragraph(&mut lines, paragraph_indentation));
    } else if !lines.is_empty() {
        result.push(take_paragraph(&mut lines, paragraph_indentation));
    }
    result
}

/// The width of the list marker `line` starts with ("2.", "(a)", "iv)",
/// "*", "-") and the spaces after it, when it starts with one followed by a
/// space.
fn list_marker(line: &[char]) -> Option<i32> {
    let word: usize = line.iter().take_while(|c| !c.is_whitespace()).count();
    let marker: String = line[..word].iter().collect();
    let is_marker = match marker.as_str() {
        "*" | "-" | "\u{2022}" => true,
        m => {
            let inner = m
                .strip_prefix('(')
                .and_then(|m| m.strip_suffix(')'))
                .or_else(|| m.strip_suffix('.'))
                .or_else(|| m.strip_suffix(')'));
            inner.is_some_and(|i| {
                !i.is_empty()
                    && i.len() <= 4
                    && (i.chars().all(|c| c.is_ascii_digit())
                        || i.chars().all(|c| c.is_ascii_alphabetic()))
            })
        }
    };
    let spaces = line[word..].iter().take_while(|&&c| c == ' ').count();
    (is_marker && spaces > 0).then(|| (word + spaces) as i32)
}

/// `_entry(...)`: a component, its license, where the full text lives, how
/// it ships, and an optional note. Each of the license, the full text and
/// how it ships is a paragraph of its own (on one line apiece: a single
/// line break would join them into one paragraph).
fn entry(
    component: &str,
    license: &str,
    full_text: &str,
    ships: Option<&str>,
    note: Option<&str>,
) -> String {
    let mut b = format!("{component}\n\nLicense: {license}\n\nFull text: {full_text}\n");
    if let Some(ships) = ships {
        b.push_str(&format!("\nShips as: {ships}\n"));
    }
    if let Some(note) = note {
        b.push_str(note);
        b.push('\n');
    }
    b.trim_end().to_string()
}

/// How a crate ships.
const STATICALLY_LINKED: &str = "statically linked into the Vocara app";

/// The generated list of the shipped crates (see the module docs).
const CRATES_JSON: &str = include_str!("../../../assets/licenses/crates.json");

#[derive(Debug, Deserialize)]
struct CrateList {
    crates: Vec<CrateRow>,
    texts: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct CrateRow {
    name: String,
    version: String,
    license: Option<String>,
    repository: Option<String>,
    texts: Vec<usize>,
}

/// The crates' entries: a header in `_entry`'s shape, then the license
/// files the crate ships (or, when it ships none, where they live).
fn crate_entries(json: &str) -> Vec<LicenseEntry> {
    // The list is generated and checked by a test; a malformed one shows no
    // crates rather than taking Settings down.
    let Ok(list) = serde_json::from_str::<CrateList>(json) else {
        return Vec::new();
    };
    list.crates
        .into_iter()
        .map(|c| {
            let texts: Vec<&str> = c
                .texts
                .iter()
                .filter_map(|&i| list.texts.get(i).map(String::as_str))
                .collect();
            let source = c
                .repository
                .clone()
                .unwrap_or_else(|| format!("https://crates.io/crates/{}", c.name));
            let full_text = if texts.is_empty() {
                source.as_str()
            } else {
                "below"
            };
            let mut text = entry(
                &format!("{} {} (Rust crate)", c.name, c.version),
                c.license.as_deref().unwrap_or("see its license file"),
                full_text,
                Some(STATICALLY_LINKED),
                None,
            );
            for t in texts {
                text.push_str("\n\n");
                text.push_str(t);
            }
            LicenseEntry {
                packages: vec![c.name],
                text,
            }
        })
        .collect()
}

/// The bundled components no crate's license covers.
fn bundled_components() -> Vec<LicenseEntry> {
    let inter = format!(
        "{}\n\n{}",
        entry(
            "Inter — the app's typeface (rsms/inter)",
            "SIL Open Font License 1.1 (OFL-1.1)",
            "below",
            Some("embedded in the Vocara app"),
            None,
        ),
        include_str!("../../../assets/fonts/inter/OFL.txt").trim_end()
    );
    vec![
        LicenseEntry {
            packages: vec!["Inter (font)".to_string()],
            text: inter,
        },
        LicenseEntry {
            packages: vec!["SQLite".to_string()],
            text: entry(
                "SQLite — the library database engine",
                "Public domain",
                "https://sqlite.org/copyright.html",
                Some("statically linked into the Vocara app (rusqlite's bundled libsqlite3-sys)"),
                None,
            ),
        },
        LicenseEntry {
            packages: vec!["Material Icons".to_string()],
            text: format!(
                "{}\n\n{}",
                entry(
                    "Material Icons — the license page's back arrow and the \
                     technique video controls' icons (google/material-design-icons)",
                    "Apache License 2.0 (Apache-2.0)",
                    "below",
                    Some(
                        "drawn by the Vocara app (the outlines of \
                         arrow_back_ios_new, rounded; volume_up, volume_down, \
                         volume_off, fullscreen and fullscreen_exit)"
                    ),
                    None,
                ),
                include_str!("../../../assets/licenses/material-icons/LICENSE").trim()
            ),
        },
        LicenseEntry {
            packages: vec!["CHIMERA".to_string()],
            text: format!(
                "{}

{}",
                entry(
                    "CHIMERA — the vocal tract view's anatomy, airway acoustics and head drawing (jamesondh/chimera-vocal-synth)",
                    "MIT License (MIT)",
                    "below",
                    Some("ported to Rust in the Vocara app (its vocara_tract crate)"),
                    Some(vocara_tract::CHIMERA_SOURCE),
                ),
                vocara_tract::CHIMERA_LICENSE.trim()
            ),
        },
        LicenseEntry {
            packages: vec!["Flutter".to_string()],
            text: format!(
                "{}\n\n{}",
                entry(
                    "Flutter — the technique video controls' play/pause \
                     animation (AnimatedIcons.play_pause, flutter/flutter)",
                    "BSD 3-Clause (BSD-3-Clause)",
                    "below",
                    Some(
                        "drawn by the Vocara app (the icon's keyframes, from \
                         Flutter 3.38's play_pause.g.dart)"
                    ),
                    None,
                ),
                include_str!("../../../assets/licenses/flutter/LICENSE").trim()
            ),
        },
    ]
}

/// What the license page lists (`_LicenseData`): the licenses, and the
/// packages they cover with the licenses bound to each.
#[derive(Debug)]
pub struct LicenseData {
    licenses: Vec<LicenseEntry>,
    packages: Vec<String>,
    bindings: HashMap<String, Vec<usize>>,
}

impl LicenseData {
    /// Collects `licenses` (`addLicense` each, then `sortPackages`).
    pub fn collect(licenses: Vec<LicenseEntry>) -> Self {
        let mut packages: Vec<String> = Vec::new();
        let mut bindings: HashMap<String, Vec<usize>> = HashMap::new();
        // Special treatment for the first package since it should be the
        // package for the delivered application.
        let mut first_package: Option<String> = None;
        for (index, license) in licenses.iter().enumerate() {
            for package in &license.packages {
                let bound = bindings.entry(package.clone()).or_insert_with(|| {
                    first_package.get_or_insert_with(|| package.clone());
                    packages.push(package.clone());
                    Vec::new()
                });
                bound.push(index);
            }
        }
        // The first package stays at the front regardless of alphabetical
        // sorting.
        packages.sort_by(|a, b| {
            if Some(a) == first_package.as_ref() && a != b {
                return std::cmp::Ordering::Less;
            }
            if Some(b) == first_package.as_ref() && a != b {
                return std::cmp::Ordering::Greater;
            }
            a.to_lowercase().cmp(&b.to_lowercase())
        });
        Self {
            licenses,
            packages,
            bindings,
        }
    }

    /// Every license.
    pub fn licenses(&self) -> &[LicenseEntry] {
        &self.licenses
    }

    /// The packages, in list order.
    pub fn packages(&self) -> &[String] {
        &self.packages
    }

    /// How many licenses are bound to `package`.
    pub fn license_count(&self, package: &str) -> usize {
        self.bindings.get(package).map_or(0, Vec::len)
    }

    /// The licenses bound to `package`.
    pub fn licenses_of(&self, package: &str) -> Vec<&LicenseEntry> {
        self.bindings
            .get(package)
            .map(|b| b.iter().filter_map(|&i| self.licenses.get(i)).collect())
            .unwrap_or_default()
    }
}

/// Every license the app ships, gathered once: the crates, then the bundled
/// components.
pub fn license_data() -> &'static LicenseData {
    static DATA: OnceLock<LicenseData> = OnceLock::new();
    DATA.get_or_init(|| {
        let mut all = crate_entries(CRATES_JSON);
        all.extend(bundled_components());
        LicenseData::collect(all)
    })
}

/// Every license the app ships (see [`license_data`]).
pub fn licenses() -> &'static [LicenseEntry] {
    license_data().licenses()
}

/// `licensesPackageDetailText`.
pub fn licenses_detail_text(count: usize) -> String {
    match count {
        0 => "No licenses.".to_string(),
        1 => "1 license.".to_string(),
        n => format!("{n} licenses."),
    }
}

// --------------------------------------------------------------------- page

/// The app's name, version and legalese, as Settings passed them.
const APPLICATION_NAME: &str = "Vocara";
const APPLICATION_LEGALESE: &str = "© 2026 Ruta Tang. All rights reserved.";

fn application_version() -> String {
    format!(
        "Version {} (build {})",
        UpdateCheckService::APP_VERSION,
        UpdateCheckService::APP_BUILD_NUMBER
    )
}

/// `kToolbarHeight`.
const TOOLBAR_HEIGHT: f32 = 56.0;
/// The master list's width (`_kMasterViewWidth`).
const MASTER_WIDTH: f32 = 320.0;
/// The gutter at 720 px wide and up (`_wideGutterSize`).
const GUTTER: f32 = 24.0;
/// `_kCardElevation`.
const CARD_ELEVATION: f32 = 4.0;
/// The about block's separation between its groups of lines
/// (`_textVerticalSeparation`'s place).
const TEXT_SEPARATION: f32 = 12.0;

/// How long a package's licenses take to come in, and the last one's to
/// go (the detail's `AnimatedSwitcher`).
pub const DETAIL_TRANSITION: Duration = Duration::from_millis(500);

/// The back button's container, for whoever has to click it.
pub const BACK: iced::widget::Id = iced::widget::Id::new("licenses-back");

/// A package's row under the pointer: the dark theme's `hoverColor`
/// (white at 4 %), the `InkWell` of a `ListTile` that sets none.
pub const TILE_HOVER: Color = Color {
    a: 0.04,
    ..Color::WHITE
};

/// The selected row's fill (`DESIGN.md`: a selected tab's, `CONTROL_ACTIVE`).
pub const TILE_SELECTED: Color = Studio::CONTROL_ACTIVE;

/// The rows' highlights' corners: a row's (`DESIGN.md`, 5).
pub const TILE_RADIUS: f32 = theme::radius::CONTROL;

/// The gathered registry, as a message carries it (compared by address).
#[derive(Clone, Copy)]
pub struct Registry(pub &'static LicenseData);

impl PartialEq for Registry {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self.0, other.0)
    }
}

impl Eq for Registry {}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Registry({} packages)", self.0.packages.len())
    }
}

/// What the page reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// The app bar's back button.
    Back,
    /// A package in the list (its index in [`LicenseData::packages`]).
    Select(usize),
    /// A frame while a package's licenses come in or go.
    Frame(Instant),
    /// The registry, gathered off the UI thread ([`LicensesPage::load`]),
    /// or why it could not be.
    Loaded(Result<Registry, String>),
    /// The package list came off its top (true) or went back to it: the
    /// app bar is scrolled under.
    MasterScrolled(bool),
}

/// The license page: what it lists once gathered, which package is shown
/// (the first until one is picked, as `selectedId` was), and the details
/// coming and going.
#[derive(Debug, Clone)]
pub struct LicensesPage {
    /// The registry, once gathered (the `FutureBuilder`'s snapshot); none
    /// while it is.
    data: Option<Result<&'static LicenseData, String>>,
    selected: usize,
    /// How many of the shown package's licenses have filled in.
    loaded: usize,
    /// The shown package's detail coming in (from the moment the registry
    /// is gathered, as the switcher brought the first one in over the empty
    /// one it started with).
    entering: Option<Controller>,
    /// The shown detail's key: a detail keeps its state (its scroll, its
    /// sheet's size) while it goes, as a keyed child of the switcher did.
    key: u64,
    /// The details going, each on its own controller running back.
    leaving: Vec<Going>,
    /// The package list is scrolled under the app bar.
    scrolled_under: bool,
    /// The last frame's time, what the drawing reads.
    now: Instant,
}

/// A package's detail going, as far as its licenses had filled in.
#[derive(Debug, Clone)]
struct Going {
    index: usize,
    key: u64,
    controller: Controller,
    loaded: usize,
}

impl Default for LicensesPage {
    fn default() -> Self {
        Self::new()
    }
}

impl LicensesPage {
    /// The page, the registry still to gather ([`LicensesPage::load`]).
    pub fn new() -> Self {
        Self {
            data: None,
            selected: 0,
            loaded: 0,
            entering: None,
            key: 0,
            leaving: Vec::new(),
            scrolled_under: false,
            now: Instant::now(),
        }
    }

    /// Gathers every license the app ships off the UI thread
    /// (`LicenseRegistry.licenses` folded into `_LicenseData`), for the
    /// page to get as [`Message::Loaded`].
    pub fn load() -> Task<Message> {
        Task::perform(crate::util::blocking(license_data), |gathered| {
            Message::Loaded(gathered.map(Registry))
        })
    }

    /// The page listing `data`, gathered now.
    pub fn over(data: &'static LicenseData) -> Self {
        let mut page = Self::new();
        page.gathered(Ok(data), Instant::now());
        page
    }

    /// What it lists, once gathered.
    fn data(&self) -> Option<&'static LicenseData> {
        match self.data {
            Some(Ok(data)) => Some(data),
            _ => None,
        }
    }

    /// How many licenses the shown package has.
    fn total(&self) -> usize {
        self.data()
            .and_then(|data| {
                data.packages
                    .get(self.selected)
                    .map(|p| data.license_count(p))
            })
            .unwrap_or(0)
    }

    /// The package shown (an index into [`LicenseData::packages`]).
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Whether the registry has been gathered.
    pub fn is_loaded(&self) -> bool {
        self.data.is_some()
    }

    /// Handles `message`; true on Back, when the page's owner closes it.
    pub fn update(&mut self, message: Message) -> bool {
        match message {
            Message::Back => true,
            Message::Select(index) => {
                self.select(index, Instant::now());
                false
            }
            Message::Frame(at) => {
                self.frame(at);
                false
            }
            Message::Loaded(gathered) => {
                self.gathered(gathered.map(|r| r.0), Instant::now());
                false
            }
            Message::MasterScrolled(under) => {
                self.scrolled_under = under;
                false
            }
        }
    }

    /// The registry, gathered at `now`: the list shows, and the first
    /// package's detail comes in (`_initDefaultDetailPage`'s post-frame
    /// callback re-keys the switcher from its empty first child). A second
    /// gathering changes nothing.
    pub fn gathered(&mut self, gathered: Result<&'static LicenseData, String>, now: Instant) {
        if self.data.is_some() {
            return;
        }
        let shows = gathered
            .as_ref()
            .is_ok_and(|data| !data.packages.is_empty());
        self.data = Some(gathered);
        if shows {
            let mut coming = Controller::new(DETAIL_TRANSITION, DETAIL_TRANSITION);
            coming.forward(now);
            self.entering = Some(coming);
        }
    }

    /// Shows package `index` from `now`: its licenses come in as the shown
    /// one's go (from wherever they had got).
    pub fn select(&mut self, index: usize, now: Instant) {
        if index == self.selected {
            return;
        }
        let mut going = self
            .entering
            .take()
            .unwrap_or_else(|| Controller::completed(DETAIL_TRANSITION, DETAIL_TRANSITION));
        going.reverse(now);
        self.leaving.retain(|g| g.index != index);
        self.leaving.push(Going {
            index: self.selected,
            key: self.key,
            controller: going,
            loaded: self.loaded,
        });
        let mut coming = Controller::new(DETAIL_TRANSITION, DETAIL_TRANSITION);
        coming.forward(now);
        self.entering = Some(coming);
        self.key += 1;
        self.selected = index;
        // A new `_PackageLicensePage`: its licenses fill in anew.
        self.loaded = 0;
    }

    /// A frame at `at`: the details move on, those gone are let go, and one
    /// more of the shown package's licenses fills in.
    pub fn frame(&mut self, at: Instant) {
        self.now = self.now.max(at);
        let now = self.now;
        self.leaving.retain(|g| g.controller.is_animating(now));
        if self.loaded < self.total() {
            self.loaded += 1;
        }
    }

    /// Whether a package's licenses come in, go or fill in at `now`.
    pub fn is_animating(&self, now: Instant) -> bool {
        self.entering.as_ref().is_some_and(|c| c.is_animating(now))
            || self.leaving.iter().any(|g| g.controller.is_animating(now))
            || self.loaded < self.total()
    }

    /// A detail on `controller` at the last frame: its opacity and how far
    /// down it is, in the page's heights (`_FadeUpwardsPageTransition`
    /// slides the switcher's whole child, the page tall).
    fn upwards(&self, controller: Option<&Controller>) -> (f32, f32) {
        let t = controller.map_or(1.0, |c| c.value(self.now));
        (
            Curve::EaseIn.transform(t),
            0.25 * (1.0 - Curve::FastOutSlowIn.transform(t)),
        )
    }

    /// The page.
    pub fn view(&self) -> Element<'static, Message> {
        let master: Element<'static, Message> = match &self.data {
            // `ConnectionState.waiting`: the about block over Material's
            // spinner, on a card with no elevation.
            None => container(column![
                about(),
                container(spinner(24.0, 2.0, Studio::TEXT_SECONDARY)).center_x(Length::Fill),
            ])
            .width(MASTER_WIDTH)
            .height(Length::Fill)
            .style(theme::fill(Studio::SURFACE))
            .into(),
            // The error, centred.
            Some(Err(error)) => container(
                text(error.clone())
                    .size(type_scale::BODY)
                    .line_height(Pixels(18.0))
                    .color(Studio::TEXT_SECONDARY),
            )
            .center_x(MASTER_WIDTH)
            .center_y(Length::Fill)
            .into(),
            Some(Ok(data)) => {
                let selected = self.selected.min(data.packages.len().saturating_sub(1));
                container(scroll_watch(
                    master(data, selected),
                    Message::MasterScrolled,
                ))
                .width(MASTER_WIDTH)
                .height(Length::Fill)
                .style(surface(Radius::from(0.0), false))
                .into()
            }
        };
        let scaffold = column![app_bar(self.scrolled_under), master]
            .width(Length::Fill)
            .height(Length::Fill);
        let mut layers = stack![container(scaffold)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::page)];
        let Some(data) = self.data() else {
            return layers.into();
        };
        let selected = self.selected.min(data.packages.len().saturating_sub(1));
        if let Some(package) = data.packages.get(selected) {
            // The details going, under the one coming in, each keeping its
            // state by its key.
            let mut details = keyed_stack();
            for going in &self.leaving {
                if let Some(package) = data.packages.get(going.index) {
                    let (opacity, down) = self.upwards(Some(&going.controller));
                    let card = detail(package, &data.licenses_of(package), going.loaded);
                    details = details.push(
                        going.key,
                        slide(opacity_layer(opacity, sheet(card, 1.0)), 0.0)
                            .down(down)
                            .inert(),
                    );
                }
            }
            let (opacity, down) = self.upwards(self.entering.as_ref());
            let card = detail(package, &data.licenses_of(package), self.loaded);
            // `_FadeUpwardsPageTransition`: the page slides, and fades as
            // one layer.
            details = details.push(
                self.key,
                slide(opacity_layer(opacity, sheet(card, 1.0)), 0.0).down(down),
            );
            // The sheet spans the page's height, over the app bar, from the
            // master's edge less the card's elevation to the gutter.
            layers = layers.push(
                container(details)
                    .padding(Padding {
                        top: 0.0,
                        right: GUTTER,
                        bottom: 0.0,
                        left: MASTER_WIDTH - CARD_ELEVATION,
                    })
                    .width(Length::Fill)
                    .height(Length::Fill),
            );
        }
        layers.into()
    }
}

/// A surface (the master, the detail card): `Studio::SURFACE` with
/// `radius`, an inset `BORDER` outline when `outlined`, and a soft shadow
/// (the card's elevation over the master).
fn surface(radius: Radius, outlined: bool) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        text_color: Some(Studio::TEXT_PRIMARY),
        background: Some(Background::Color(Studio::SURFACE)),
        border: Border {
            radius,
            color: Studio::BORDER,
            width: if outlined { 1.0 } else { 0.0 },
        },
        shadow: Shadow {
            color: Color::from_rgba8(0, 0, 0, 0.35),
            offset: Vector::new(0.0, 2.0),
            blur_radius: 8.0,
        },
        snap: false,
    }
}

/// `color` under the scheme's surface tint at Material 3's scrolled-under
/// elevation (`ElevationOverlay.applySurfaceTint`: `primary` at 8 %
/// alpha-blended over it).
pub fn scrolled_under(color: Color) -> Color {
    let t = SCROLLED_UNDER_TINT;
    let tint = Zinc::PRIMARY;
    Color {
        r: tint.r * t + color.r * (1.0 - t),
        g: tint.g * t + color.g * (1.0 - t),
        b: tint.b * t + color.b * (1.0 - t),
        a: 1.0,
    }
}

/// The app bar: the page's toolbar (the back button, then "Licenses"),
/// over an empty second row (the master/detail scaffold's action row).
/// Once the package list is scrolled under it, that row's `Studio.surface`
/// takes the surface tint, at once (the bar's `Material` does not animate
/// its color).
fn app_bar<'a>(scrolled: bool) -> Element<'a, Message> {
    let background = if scrolled {
        scrolled_under(Studio::SURFACE)
    } else {
        Studio::SURFACE
    };
    let bar = row![
        container(toolbar::back_button(Message::Back)).id(BACK),
        Space::new().width(8),
        toolbar::title("Licenses"),
    ]
    .align_y(iced::alignment::Vertical::Center);
    column![
        toolbar::page_toolbar(bar),
        container(Space::new().height(TOOLBAR_HEIGHT))
            .width(Length::Fill)
            .style(theme::fill(background)),
    ]
    .width(Length::Fill)
    .into()
}

/// `_AboutProgram`: the name (15 px semibold), the version (12 px
/// `TEXT_SECONDARY`), the legalese and "Powered by" (11 px `TEXT_FAINT`).
fn about<'a>() -> Element<'a, Message> {
    let centred = |s: String, size: f32, line: f32, color: Color| {
        text(s)
            .size(size)
            .line_height(Pixels(line))
            .color(color)
            .width(Length::Fill)
            .align_x(Horizontal::Center)
    };
    container(column![
        centred(
            APPLICATION_NAME.to_string(),
            type_scale::HEADING,
            20.0,
            Studio::TEXT_PRIMARY
        )
        .font(font::SEMIBOLD),
        container(centred(
            application_version(),
            type_scale::BODY,
            type_scale::LINE,
            Studio::TEXT_SECONDARY
        ))
        .padding(Padding {
            bottom: TEXT_SEPARATION,
            ..Padding::ZERO
        }),
        centred(
            APPLICATION_LEGALESE.to_string(),
            type_scale::LABEL,
            type_scale::LINE,
            Studio::TEXT_FAINT
        ),
        Space::new().height(TEXT_SEPARATION),
        centred(
            "Powered by iced".to_string(),
            type_scale::LABEL,
            type_scale::LINE,
            Studio::TEXT_FAINT
        ),
    ])
    .padding([24.0, GUTTER])
    .width(Length::Fill)
    .into()
}

/// The about header and the package list (`_PackagesView`).
fn master(data: &LicenseData, selected: usize) -> Element<'_, Message> {
    let mut list = Column::new().push(about());
    for (index, package) in data.packages.iter().enumerate() {
        let count = data.bindings.get(package).map_or(0, Vec::len);
        list = list.push(package_tile(package, count, index, index == selected));
    }
    scrollable(list.width(Length::Fill))
        .height(Length::Fill)
        .fading()
        .into()
}

/// `_PackageListTile`: the package and its license count in a two-line
/// `ListTile` (64 tall at the desktop's compact density, its baselines 32
/// and 48 down), on [`TILE_SELECTED`] when selected (an `Ink`
/// under the tile's own). Its `InkWell` lights the row in forui's rounded
/// list-tile shape over that: the theme's `hoverColor` under the pointer
/// (fading over 50 ms) and its `highlightColor` while pressed (over
/// 200 ms). The row is built alike either way, so its highlights carry
/// over a change of selection.
fn package_tile<'a>(
    package: &str,
    licenses: usize,
    index: usize,
    selected: bool,
) -> Element<'a, Message> {
    let tile = two_line_tile(
        text(package.to_string())
            .size(list_tile::TITLE_SIZE)
            .color(Studio::TEXT_PRIMARY),
        text(licenses_detail_text(licenses))
            .size(list_tile::SUBTITLE_SIZE)
            .color(Studio::TEXT_FAINT),
    );
    let row = button(tile)
        .padding(0)
        .width(Length::Fill)
        .style(theme::bare_button)
        .on_press(Message::Select(index));
    container(ink_hover(row, TILE_HOVER, TILE_RADIUS))
        .width(Length::Fill)
        .style(move |_: &Theme| container::Style {
            background: selected.then_some(Background::Color(TILE_SELECTED)),
            ..container::Style::default()
        })
        .into()
}

/// `_PackageLicensePage` on the detail card: the package and its license
/// count, then each of the first `loaded` licenses after a divider,
/// paragraph by paragraph, and Material's spinner while more are to come.
fn detail<'a>(package: &str, licenses: &[&LicenseEntry], loaded: usize) -> Element<'a, Message> {
    let fg = Studio::TEXT_SECONDARY;
    // `_PackageLicensePageTitle` in the pinned `SliverAppBar`'s toolbar:
    // the title (13 px semibold) right over the subtitle (11 px
    // `TEXT_FAINT`), both at the column's start, the column centred on the
    // card (macOS, no actions) and in the toolbar's height.
    let header = container(column![
        text(package.to_string())
            .size(type_scale::TITLE)
            .line_height(Pixels(18.0))
            .font(font::SEMIBOLD)
            .color(Studio::TEXT_PRIMARY)
            .wrapping(Wrapping::None),
        text(licenses_detail_text(licenses.len()))
            .size(type_scale::LABEL)
            .line_height(Pixels(type_scale::LINE))
            .color(Studio::TEXT_FAINT)
            .wrapping(Wrapping::None),
    ])
    .padding([0.0, 16.0])
    .center_x(Length::Fill)
    .center_y(TOOLBAR_HEIGHT);
    let mut body = Column::new();
    for license in licenses.iter().take(loaded) {
        // `Padding(all: 18, child: Divider())`: a 16 px divider, its 1 px
        // line centred, in `Studio::BORDER`.
        body = body.push(
            container(
                container(Space::new())
                    .width(Length::Fill)
                    .height(1)
                    .style(theme::fill(Studio::BORDER)),
            )
            .padding([18.0 + 7.5, 18.0]),
        );
        for p in license.paragraphs() {
            body = body.push(paragraph(p, fg));
        }
    }
    if loaded < licenses.len() {
        // A spinner in 24 / 0 padding, neutral.
        body = body.push(
            container(spinner(24.0, 2.0, Studio::TEXT_SECONDARY))
                .width(Length::Fill)
                .align_x(Horizontal::Center)
                .padding([24, 0]),
        );
    }
    let content = column![
        header,
        scrollable(body.padding(Padding {
            top: 0.0,
            right: GUTTER,
            bottom: GUTTER,
            left: GUTTER,
        }))
        .height(Length::Fill)
        .fading(),
    ];
    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(surface(
            Radius {
                top_left: theme::radius::CARD,
                top_right: theme::radius::CARD,
                bottom_right: 0.0,
                bottom_left: 0.0,
            },
            true,
        ))
        .into()
}

/// One paragraph of a license, 12 px on 16 px lines in `color`: a centred
/// semibold heading (in `TEXT_PRIMARY`) 16 below the last, or text 8 below
/// it, indented 16 a level.
fn paragraph<'a>(p: LicenseParagraph, color: Color) -> Element<'a, Message> {
    let body = text(p.text)
        .size(type_scale::BODY)
        .line_height(LineHeight::Absolute(Pixels(type_scale::LINE)))
        .color(color)
        .width(Length::Fill);
    if p.indent == LicenseParagraph::CENTERED_INDENT {
        container(
            body.font(font::SEMIBOLD)
                .color(Studio::TEXT_PRIMARY)
                .align_x(Horizontal::Center),
        )
        .padding(Padding {
            top: 16.0,
            ..Padding::ZERO
        })
        .width(Length::Fill)
        .into()
    } else {
        container(body)
            .padding(Padding {
                top: 8.0,
                left: 16.0 * p.indent.max(0) as f32,
                ..Padding::ZERO
            })
            .width(Length::Fill)
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn para(text: &str, indent: i32) -> LicenseParagraph {
        LicenseParagraph {
            text: text.to_string(),
            indent,
        }
    }

    // Flutter's licenses_test: "LicenseEntryWithLineBreaks - most cases".
    #[test]
    fn paragraphs_split_as_flutter_split_them() {
        let text = "A\nA\nA\n  B\nB\nB\n   C\nC\nC\n                  D\nD\nD\n\nE\nE\n F\n  G\n G\nG\n\n[H\n H\n H]\n  \nI\u{c}J\n\u{c}K\nK\n\u{c}\nL\nL L\nL  L\nL   L\nL    L\nL     L\n\n   M\nM   \nM    \n\nN\n\nO\nO\n\n\nP\n\n\n\nQQQ\n\nRR RRR RRRR RRRRR\nR\n\nS\n\n   T\n\n      U\n         V\n\n        W\n\n       X\n      \n      Y";
        let c = LicenseParagraph::CENTERED_INDENT;
        assert_eq!(
            license_paragraphs(text),
            vec![
                para("A A A", 0),
                para("B B B", 0),
                para("C C C", 1),
                para("D D D", c),
                para("E E", 0),
                para("F", 0),
                para("G G G", 0),
                para("[H H H]", 0),
                para("I", 0),
                para("J", 0),
                para("K K", 0),
                para("L L L L  L L   L L    L L     L", 0),
                para("M M    M    ", 1),
                para("N", 0),
                para("O O", 0),
                para("P", 0),
                para("QQQ", 0),
                para("RR RRR RRRR RRRRR R", 0),
                para("S", 0),
                para("T", 1),
                para("U", 2),
                para("V", 3),
                para("W", 2),
                para("X", 2),
                para("Y", 2),
            ]
        );
    }

    /// A list item's lines hang under its text, and go on with the item:
    /// the Apache License's numbered sections and lettered conditions and
    /// the BSD licenses' bullets read as one paragraph each, where Flutter
    /// broke them after their first line, mid-sentence.
    #[test]
    fn a_list_items_hanging_lines_go_on_with_it() {
        let apache = "   2. Grant of Copyright License. Subject to the terms and conditions of\n      this License, each Contributor hereby grants to You a perpetual,\n      worldwide license.\n\n      (a) You must give any other recipients of the Work or\n          Derivative Works a copy of this License; and\n\n      (b) You must cause any modified files to carry prominent notices\n          stating that You changed the files; and\n";
        assert_eq!(
            license_paragraphs(apache),
            vec![
                para(
                    "2. Grant of Copyright License. Subject to the terms and conditions of \
                     this License, each Contributor hereby grants to You a perpetual, \
                     worldwide license.",
                    1
                ),
                para(
                    "(a) You must give any other recipients of the Work or Derivative \
                     Works a copy of this License; and",
                    2
                ),
                para(
                    "(b) You must cause any modified files to carry prominent notices \
                     stating that You changed the files; and",
                    2
                ),
            ]
        );
        let bsd = "    * Redistributions of source code must retain the above copyright\n      notice, this list of conditions and the following disclaimer.\n    * Neither the name of the copyright holder nor the names of its\n      contributors may be used.\n";
        assert_eq!(
            license_paragraphs(bsd),
            vec![
                para(
                    "* Redistributions of source code must retain the above copyright \
                     notice, this list of conditions and the following disclaimer.",
                    1
                ),
                para(
                    "* Neither the name of the copyright holder nor the names of its \
                     contributors may be used.",
                    1
                ),
            ]
        );
        // A nested item at the hang starts a paragraph of its own; a line
        // indented past the text of a line that is no item still does.
        assert_eq!(
            license_paragraphs("   1. Terms\n      (a) first\n"),
            vec![para("1. Terms", 1), para("(a) first", 2)]
        );
        assert_eq!(
            license_paragraphs("Title\n   more\n"),
            vec![para("Title", 0), para("more", 1)]
        );
    }

    // "LicenseEntryWithLineBreaks - leading and trailing whitespace".
    #[test]
    fn leading_and_trailing_whitespace_makes_no_paragraph() {
        assert!(license_paragraphs("    \n\n    ").is_empty());
        assert!(license_paragraphs("    \r\n\r\n    ").is_empty());
        assert_eq!(license_paragraphs("    \nA\n    "), vec![para("A", 0)]);
        assert_eq!(license_paragraphs("\n\n\nA\n\n\n"), vec![para("A", 0)]);
    }

    #[test]
    fn the_registry_carries_the_shipped_crates_and_the_bundled_components() {
        let data = license_data();
        let packages = data.packages();
        for package in [
            "Inter (font)",
            "SQLite",
            "Material Icons",
            "Flutter",
            "CHIMERA",
            "iced",
            "rusqlite",
            "serde",
            "lucide-icons",
            // The engine's crates, through the device feature.
            "cpal",
            "symphonia",
            "objc2-core-audio",
        ] {
            assert!(packages.iter().any(|p| p == package), "{package} is listed");
        }
        // First-party crates are not.
        for own in ["vocara_app", "vocara_model", "vocara_engine"] {
            assert!(!packages.iter().any(|p| p == own), "{own} is not listed");
        }
        // Gathered once: the same registry every time.
        assert!(std::ptr::eq(license_data(), data));
        assert_eq!(licenses().len(), data.licenses().len());
        // Every crate entry says what it is and how it ships.
        let iced = data.licenses_of("iced");
        assert!(!iced.is_empty());
        assert!(iced[0].text.starts_with("iced "));
        assert!(iced[0].text.contains("Ships as: statically linked"));
        // The OFL ships with the font.
        let inter = data.licenses_of("Inter (font)");
        assert!(inter[0].text.contains("SIL OPEN FONT LICENSE Version 1.1"));
        // The back button's arrow is Material Icons' outline, under its
        // Apache License, in full.
        let icons = data.licenses_of("Material Icons");
        assert_eq!(icons.len(), 1);
        assert!(icons[0].text.contains("License: Apache License 2.0"));
        assert!(icons[0].text.contains("arrow_back_ios_new"));
        assert!(icons[0].text.contains("fullscreen_exit"));
        assert!(icons[0]
            .text
            .contains("TERMS AND CONDITIONS FOR USE, REPRODUCTION, AND DISTRIBUTION"));
        // The vocal tract's model is CHIMERA's, ported: its MIT notice, in
        // full.
        let chimera = data.licenses_of("CHIMERA");
        assert_eq!(chimera.len(), 1);
        assert!(chimera[0].text.contains("License: MIT License"));
        assert!(chimera[0].text.contains("Copyright (c) 2026 Jameson Hodge"));
        assert!(chimera[0]
            .text
            .contains("153c7c8f22108c400e8556950a2c3b61f3f62aa2"));
        // Its title reads as one sentence (no run of spaces from a wrapped
        // literal).
        assert!(chimera[0]
            .text
            .contains("airway acoustics and head drawing"));
        // The video controls' play/pause keyframes are Flutter's, under its
        // BSD license, in full.
        let flutter = data.licenses_of("Flutter");
        assert_eq!(flutter.len(), 1);
        assert!(flutter[0].text.contains("AnimatedIcons.play_pause"));
        assert!(flutter[0]
            .text
            .contains("Copyright 2014 The Flutter Authors. All rights reserved."));
    }

    #[test]
    fn packages_sort_case_insensitively_with_the_first_registered_first() {
        let entry = |packages: &[&str]| LicenseEntry {
            packages: packages.iter().map(|p| p.to_string()).collect(),
            text: "MIT".to_string(),
        };
        let data = LicenseData::collect(vec![
            entry(&["zeta"]),
            entry(&["Beta", "alpha"]),
            entry(&["alpha"]),
            entry(&["Gamma"]),
        ]);
        assert_eq!(data.packages(), ["zeta", "alpha", "Beta", "Gamma"]);
        assert_eq!(data.licenses_of("alpha").len(), 2);
        assert_eq!(data.licenses_of("zeta").len(), 1);
        assert!(data.licenses_of("nope").is_empty());
        assert_eq!(licenses_detail_text(0), "No licenses.");
        assert_eq!(licenses_detail_text(1), "1 license.");
        assert_eq!(licenses_detail_text(2), "2 licenses.");
    }

    #[test]
    fn a_crate_with_no_license_file_points_at_its_source() {
        let json = r#"{"crates": [
            {"name": "a", "version": "1.0.0", "license": "MIT", "repository": "https://example.com/a", "texts": [0]},
            {"name": "b", "version": "0.2.0", "license": "Zlib", "repository": null, "texts": []},
            {"name": "c", "version": "3.0.0", "license": null, "repository": null, "texts": [7]}
        ], "texts": ["Copyright A\n\nPermission is hereby granted"]}"#;
        let entries = crate_entries(json);
        assert_eq!(entries.len(), 3);
        assert_eq!(
            entries[0].text,
            "a 1.0.0 (Rust crate)\n\nLicense: MIT\n\nFull text: below\n\nShips as: statically linked into the Vocara app\n\nCopyright A\n\nPermission is hereby granted"
        );
        // The header's lines are paragraphs of their own, each on its own
        // line (they ran together: "License: MIT Full text: below ...").
        let paragraphs: Vec<String> = entries[0]
            .paragraphs()
            .into_iter()
            .map(|p| p.text)
            .collect();
        assert_eq!(
            paragraphs[..4],
            [
                "a 1.0.0 (Rust crate)",
                "License: MIT",
                "Full text: below",
                "Ships as: statically linked into the Vocara app",
            ]
        );
        assert!(entries[1]
            .text
            .contains("Full text: https://crates.io/crates/b"));
        assert!(entries[2].text.contains("License: see its license file"));
        assert!(crate_entries("not json").is_empty());
    }

    #[test]
    fn the_page_lists_the_packages_and_shows_the_selected_one() {
        let data = LicenseData::collect(vec![
            LicenseEntry {
                packages: vec!["first".into()],
                text: "First license\n\n        CENTRED HEADING\n\n   indented".into(),
            },
            LicenseEntry {
                packages: vec!["second".into()],
                text: "Second license".into(),
            },
            LicenseEntry {
                packages: vec!["second".into()],
                text: "Another".into(),
            },
        ]);
        let data: &'static LicenseData = Box::leak(Box::new(data));
        let mut page = LicensesPage::over(data);
        // Its licenses fill in, one a frame.
        let t0 = Instant::now();
        page.frame(t0);
        let messages: Vec<Message> = {
            let mut ui = iced_test::simulator(page.view());
            assert!(ui.find("Licenses").is_ok());
            assert!(ui.find("Vocara").is_ok());
            assert!(ui.find(application_version().as_str()).is_ok());
            assert!(ui.find("© 2026 Ruta Tang. All rights reserved.").is_ok());
            assert!(ui.find("Powered by iced").is_ok());
            assert!(ui.find("2 licenses.").is_ok());
            // The first package is shown until another is picked.
            assert!(ui.find("First license").is_ok());
            assert!(ui.find("Second license").is_err());
            ui.click("second").expect("the second package");
            ui.into_messages().collect()
        };
        assert_eq!(messages, vec![Message::Select(1)]);
        for m in messages {
            assert!(!page.update(m));
        }
        assert_eq!(page.selected(), 1);
        page.frame(t0 + DETAIL_TRANSITION);
        page.frame(t0 + DETAIL_TRANSITION);
        let mut ui = iced_test::simulator(page.view());
        assert!(ui.find("Second license").is_ok());
        assert!(ui.find("Another").is_ok());
        assert!(ui.find("First license").is_err());

        let messages: Vec<Message> = {
            let mut ui = iced_test::simulator(page.view());
            ui.click(BACK).expect("the back button");
            ui.into_messages().collect()
        };
        assert_eq!(messages, vec![Message::Back]);
        assert!(page.update(Message::Back));
    }

    #[test]
    fn a_picked_packages_licenses_come_up_and_in_as_the_last_ones_go() {
        let mut page = LicensesPage::new();
        let t = Instant::now();
        page.gathered(Ok(license_data()), t);
        // Say its licenses have filled in (see the next test).
        page.loaded = page.total();
        let at = |page: &mut LicensesPage, now: Instant| {
            page.now = now;
            let coming = page.upwards(page.entering.as_ref());
            let going = page.upwards(page.leaving.first().map(|g| &g.controller));
            (coming, going)
        };
        // The first comes in the same way, from the empty detail the
        // switcher started with.
        let ((opacity, down), _) = at(&mut page, t);
        assert_eq!((opacity, down), (0.0, 0.25), "the first: down, clear");
        assert!(page.is_animating(t));
        let t0 = t + DETAIL_TRANSITION;
        let ((opacity, down), _) = at(&mut page, t0);
        assert_eq!((opacity, down), (1.0, 0.0), "in");
        assert!(!page.is_animating(t0));
        page.select(1, t0);
        assert_eq!(page.selected(), 1, "at once");
        assert!(page.is_animating(t0 + DETAIL_TRANSITION / 2));
        let at = |page: &mut LicensesPage, d: Duration| at(page, t0 + d);
        let ((opacity, down), (gone, sunk)) = at(&mut page, Duration::ZERO);
        assert_eq!((opacity, down), (0.0, 0.25), "start: down a quarter, clear");
        assert_eq!((gone, sunk), (1.0, 0.0), "the last one in full");
        let ((opacity, down), (gone, sunk)) = at(&mut page, DETAIL_TRANSITION / 2);
        assert!(
            (opacity - Curve::EaseIn.transform(0.5)).abs() < 1e-3,
            "{opacity}"
        );
        let rise = 0.25 * (1.0 - Curve::FastOutSlowIn.transform(0.5));
        assert!((down - rise).abs() < 1e-3, "{down}");
        assert!(
            (gone - opacity).abs() < 1e-3 && (sunk - down).abs() < 1e-3,
            "mirrored"
        );
        let ((opacity, down), _) = at(&mut page, DETAIL_TRANSITION);
        assert_eq!((opacity, down), (1.0, 0.0), "end");
        page.frame(t0 + DETAIL_TRANSITION);
        assert!(page.leaving.is_empty(), "the last one let go");
        page.loaded = page.total();
        assert!(!page.is_animating(t0 + DETAIL_TRANSITION));
    }

    #[test]
    fn a_packages_licenses_fill_in_one_a_frame_under_a_spinner() {
        let entry = |text: &str| LicenseEntry {
            packages: vec!["three".into()],
            text: text.into(),
        };
        let data = LicenseData::collect(vec![entry("One"), entry("Two"), entry("Three")]);
        let data: &'static LicenseData = Box::leak(Box::new(data));
        let mut page = LicensesPage::over(data);
        let shown = |page: &LicensesPage| {
            let mut ui = iced_test::simulator(page.view());
            ["One", "Two", "Three"].map(|t| ui.find(t).is_ok())
        };
        let t0 = Instant::now();
        assert!(page.is_animating(t0), "frames while they fill in");
        assert_eq!(shown(&page), [false; 3], "none yet");
        assert!(iced_test::simulator(page.view())
            .find("3 licenses.")
            .is_ok());
        page.frame(t0);
        assert_eq!(shown(&page), [true, false, false]);
        page.frame(t0);
        page.frame(t0);
        assert_eq!(shown(&page), [true; 3], "all in");
        assert!(
            !page.is_animating(t0 + DETAIL_TRANSITION),
            "and the frames stop once the detail is in"
        );
        page.frame(t0);
        assert_eq!(page.loaded, 3);
    }

    #[test]
    fn the_real_page_renders() {
        let mut page = LicensesPage::new();
        let render = |page: &LicensesPage| {
            let mut ui = iced_test::Simulator::with_size(
                iced::Settings::default(),
                iced::Size::new(1440.0, 872.0),
                page.view(),
            );
            ui.snapshot(&theme::app_theme()).expect("the page renders");
        };
        render(&page);
        page.gathered(Ok(license_data()), Instant::now());
        page.frame(Instant::now() + DETAIL_TRANSITION);
        render(&page);
    }
}
