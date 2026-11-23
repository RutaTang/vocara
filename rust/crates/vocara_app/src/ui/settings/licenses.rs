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
//!   typeface the app embeds (its OFL text ships with the font) and SQLite,
//!   which rusqlite compiles in. Keep this list in sync when the app starts
//!   shipping another non-crate component (a media framework for the
//!   technique videos, say). The engine's own code is first-party, covered
//!   by the app's copyright; its crates are in the generated list.
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
//! `Studio.surface` cards and app bar) over forui's zinc text styles. Flutter
//! faded a newly selected package in over 500 ms and loaded its paragraphs
//! a few per frame; iced shows them at once. "Powered by Flutter" reads
//! "Powered by iced".

use std::collections::HashMap;
use std::sync::OnceLock;

use iced::alignment::{Horizontal, Vertical};
use iced::border::Radius;
use iced::widget::text::{LineHeight, Wrapping};
use iced::widget::{
    button, column, container, keyed_column, row, scrollable, stack, text, Column, Space,
};
use iced::{Background, Border, Color, Element, Length, Padding, Shadow, Theme, Vector};
use serde::Deserialize;
use vocara_services::update_check_service::UpdateCheckService;

use crate::ui::core::icons::{self, Icon};
use crate::ui::core::theme::{self, alpha, font, Studio, Zinc};

#[cfg(test)]
mod generate;

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
pub fn license_paragraphs(text: &str) -> Vec<LicenseParagraph> {
    let chars: Vec<char> = text.chars().collect();
    let mut line_start = 0usize;
    let mut position = 0usize;
    let mut last_line_indent = 0i32;
    let mut current_line_indent = 0i32;
    let mut paragraph_indentation: Option<i32> = None;
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
                    if !lines.is_empty() && current_line_indent > last_line_indent {
                        result.push(take_paragraph(&mut lines, paragraph_indentation));
                        paragraph_indentation = None;
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

/// `_entry(...)`: a component, its license, where the full text lives, how
/// it ships, and an optional note.
fn entry(
    component: &str,
    license: &str,
    full_text: &str,
    ships: Option<&str>,
    note: Option<&str>,
) -> String {
    let mut b = format!("{component}\n\nLicense: {license}\nFull text: {full_text}\n");
    if let Some(ships) = ships {
        b.push_str(&format!("Ships as: {ships}\n"));
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
/// `_textVerticalSeparation`.
const TEXT_SEPARATION: f32 = 18.0;

/// What the page reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// The app bar's back button.
    Back,
    /// A package in the list (its index in [`LicenseData::packages`]).
    Select(usize),
}

/// The license page: which package is shown (the first until one is
/// picked, as `selectedId` was).
#[derive(Debug, Default, Clone)]
pub struct LicensesPage {
    selected: usize,
}

impl LicensesPage {
    pub fn new() -> Self {
        Self::default()
    }

    /// The package shown (an index into [`LicenseData::packages`]).
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Handles `message`; true on Back, when the page's owner closes it.
    pub fn update(&mut self, message: Message) -> bool {
        match message {
            Message::Back => true,
            Message::Select(index) => {
                self.selected = index;
                false
            }
        }
    }

    /// The page, listing every license the app ships.
    pub fn view(&self) -> Element<'_, Message> {
        self.view_of(license_data())
    }

    /// The page over `data`.
    pub fn view_of<'a>(&self, data: &'a LicenseData) -> Element<'a, Message> {
        let selected = self.selected.min(data.packages.len().saturating_sub(1));
        let scaffold = column![
            app_bar(),
            container(master(data, selected))
                .width(MASTER_WIDTH)
                .height(Length::Fill)
                .style(material(Some(Radius::from(0.0)))),
        ]
        .width(Length::Fill)
        .height(Length::Fill);
        let mut layers = stack![container(scaffold)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::page)];
        if let Some(package) = data.packages.get(selected) {
            // The detail card sits over the app bar's lower half, from the
            // master's edge (less the card's elevation) to the gutter.
            layers = layers.push(
                container(
                    keyed_column([(selected, detail(package, &data.licenses_of(package)))])
                        .height(Length::Fill),
                )
                .padding(Padding {
                    top: TOOLBAR_HEIGHT,
                    right: GUTTER + CARD_ELEVATION,
                    bottom: 0.0,
                    left: MASTER_WIDTH,
                })
                .width(Length::Fill)
                .height(Length::Fill),
            );
        }
        layers.into()
    }
}

/// A Material surface: `Studio.surface` (the card colour Settings set) at
/// elevation 4, with `radius` (`None` = square).
fn material(radius: Option<Radius>) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        text_color: Some(Zinc::FOREGROUND),
        background: Some(Background::Color(Studio::SURFACE)),
        border: Border {
            radius: radius.unwrap_or_default(),
            ..Border::default()
        },
        shadow: Shadow {
            color: Color::from_rgba8(0, 0, 0, 0.45),
            offset: Vector::new(0.0, 2.0),
            blur_radius: 6.0,
        },
        snap: false,
    }
}

/// The app bar: a back button and "Licenses" on the toolbar, over an empty
/// second toolbar row (the master/detail scaffold's action row).
fn app_bar<'a>() -> Element<'a, Message> {
    let back = button(icons::icon(Icon::ChevronLeft, 24.0, Studio::TEXT_PRIMARY))
        .padding(12)
        .style(|theme: &Theme, status| button::Style {
            background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                .then_some(Background::Color(alpha(Studio::TEXT_PRIMARY, 0.08))),
            border: Border {
                radius: 24.0.into(),
                ..Border::default()
            },
            ..theme::bare_button(theme, status)
        })
        .on_press(Message::Back);
    container(column![
        row![
            container(back)
                .center_x(TOOLBAR_HEIGHT)
                .center_y(TOOLBAR_HEIGHT),
            text("Licenses")
                .size(18)
                .line_height(1.0)
                .color(Studio::TEXT_PRIMARY),
        ]
        .height(TOOLBAR_HEIGHT)
        .align_y(Vertical::Center),
        Space::new().height(TOOLBAR_HEIGHT),
    ])
    .width(Length::Fill)
    .style(theme::fill(Studio::SURFACE))
    .into()
}

/// `_AboutProgram`: the name, version, legalese and "Powered by".
fn about<'a>() -> Element<'a, Message> {
    let centred = |s: String, size: f32| {
        text(s)
            .size(size)
            .line_height(1.0)
            .color(Zinc::FOREGROUND)
            .width(Length::Fill)
            .align_x(Horizontal::Center)
    };
    container(column![
        centred(APPLICATION_NAME.to_string(), 20.0),
        container(centred(application_version(), 14.0)).padding(Padding {
            bottom: TEXT_SEPARATION,
            ..Padding::ZERO
        }),
        centred(APPLICATION_LEGALESE.to_string(), 12.0),
        Space::new().height(TEXT_SEPARATION),
        centred("Powered by iced".to_string(), 14.0),
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
        .style(theme::scrollbar)
        .into()
}

/// The dark theme's `highlightColor`: a selected package's background.
const HIGHLIGHT: Color = Color::from_rgba8(0xCC, 0xCC, 0xCC, 0x40 as f32 / 255.0);

/// `_PackageListTile`: the package and its license count, 72 tall.
fn package_tile<'a>(
    package: &str,
    licenses: usize,
    index: usize,
    selected: bool,
) -> Element<'a, Message> {
    let content = column![
        text(package.to_string())
            .size(16)
            .line_height(1.0)
            .color(Zinc::FOREGROUND)
            .wrapping(Wrapping::None),
        Space::new().height(4),
        text(licenses_detail_text(licenses))
            .size(14)
            .line_height(1.0)
            .color(Zinc::FOREGROUND),
    ];
    button(
        container(content)
            .height(72)
            .width(Length::Fill)
            .padding(Padding {
                top: 0.0,
                right: 24.0,
                bottom: 0.0,
                left: 16.0,
            })
            .align_y(Vertical::Center)
            .clip(true),
    )
    .padding(0)
    .width(Length::Fill)
    .style(move |_: &Theme, status| {
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        let background = if selected {
            Some(HIGHLIGHT)
        } else if hovered {
            Some(alpha(Zinc::FOREGROUND, 0.04))
        } else {
            None
        };
        button::Style {
            background: background.map(Background::Color),
            text_color: Zinc::FOREGROUND,
            border: Border::default(),
            shadow: Shadow::default(),
            snap: false,
        }
    })
    .on_press(Message::Select(index))
    .into()
}

/// `_PackageLicensePage` on the detail card: the package and its license
/// count, then each license after a divider, paragraph by paragraph.
fn detail<'a>(package: &str, licenses: &[&LicenseEntry]) -> Element<'a, Message> {
    let header = container(column![
        text(package.to_string())
            .size(18)
            .line_height(1.0)
            .color(Zinc::FOREGROUND),
        Space::new().height(4),
        text(licenses_detail_text(licenses.len()))
            .size(14)
            .line_height(1.0)
            .color(Zinc::FOREGROUND),
    ])
    .height(TOOLBAR_HEIGHT)
    .padding([0.0, 16.0])
    .align_y(Vertical::Center);
    let mut body = Column::new();
    for license in licenses {
        // `Padding(all: 18, child: Divider())`: a 16 px divider, its 1 px
        // line centred (in the scheme's outline colour, the foreground).
        body = body.push(
            container(
                container(Space::new())
                    .width(Length::Fill)
                    .height(1)
                    .style(theme::fill(Zinc::FOREGROUND)),
            )
            .padding([18.0 + 7.5, 18.0]),
        );
        for p in license.paragraphs() {
            body = body.push(paragraph(p));
        }
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
        .style(theme::scrollbar),
    ];
    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(material(Some(Radius {
            top_left: 3.0,
            top_right: 3.0,
            bottom_right: 0.0,
            bottom_left: 0.0,
        })))
        .into()
}

/// One paragraph in the licenses' `bodySmall` style: a centred bold heading
/// 16 below the last, or text 8 below it, indented 16 a level.
fn paragraph<'a>(p: LicenseParagraph) -> Element<'a, Message> {
    let body = text(p.text)
        .size(12)
        .line_height(LineHeight::Relative(1.0))
        .color(Zinc::FOREGROUND)
        .width(Length::Fill);
    if p.indent == LicenseParagraph::CENTERED_INDENT {
        container(body.font(font::BOLD).align_x(Horizontal::Center))
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
            "a 1.0.0 (Rust crate)\n\nLicense: MIT\nFull text: below\nShips as: statically linked into the Vocara app\n\nCopyright A\n\nPermission is hereby granted"
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
        let mut page = LicensesPage::new();
        let messages: Vec<Message> = {
            let mut ui = iced_test::simulator(page.view_of(&data));
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
        let mut ui = iced_test::simulator(page.view_of(&data));
        assert!(ui.find("Second license").is_ok());
        assert!(ui.find("Another").is_ok());
        assert!(ui.find("First license").is_err());

        let back = char::from(Icon::ChevronLeft).to_string();
        let messages: Vec<Message> = {
            let mut ui = iced_test::simulator(page.view_of(&data));
            ui.click(back.as_str()).expect("the back button");
            ui.into_messages().collect()
        };
        assert_eq!(messages, vec![Message::Back]);
        assert!(page.update(Message::Back));
    }

    #[test]
    fn the_real_page_renders() {
        let page = LicensesPage::new();
        let mut ui = iced_test::Simulator::with_size(
            iced::Settings::default(),
            iced::Size::new(1440.0, 872.0),
            page.view(),
        );
        ui.snapshot(&theme::app_theme()).expect("the page renders");
    }
}
