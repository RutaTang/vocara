//! Port of `ui/home/widgets/update_notice.dart`, grown into the self-update's
//! face: the banner under the library's toolbar ([`update_notice_banner`]),
//! the update dialog for a newer release ([`UpdateDownloadDialog`], which
//! Settings opens too), and [`open_url_in_browser`].
//!
//! Both read the update check (`UpdateCheckService`: is there a newer
//! release) and the self-update (`SelfUpdate`: where the update stands) as
//! they are whenever iced draws them; the app redraws on every change of the
//! self-update (its listeners), and the screens on the check's results.
//! What they show, by the self-update's phase:
//!
//! - **The offer**: "Vocara 0.4.0 is available." with "What's new" (the
//!   dialog, its release notes) and "Update now", the one click that
//!   downloads, verifies, installs and relaunches. Where this copy cannot
//!   replace itself (not an installed app, a read-only volume, a translocated
//!   copy, no Developer ID signature) or the release names no image, the
//!   offer is "Download", and the dialog says why and offers the release
//!   page ("Open Download Page"), as before the self-update.
//! - **Downloading**: the bytes so far of the image's size, a progress bar,
//!   and Cancel.
//! - **Installing**: "Installing Vocara 0.4.0… Vocara will relaunch." — the
//!   image is checked and staged, and then the app quits through its quit
//!   guard and the helper relaunches it.
//! - **Failed**: "Update failed: <why>", with "Try again" (and the dialog's
//!   "Open Download Page"), whether it failed now or in the swap after the
//!   last quit (the relaunched Vocara reports that once).
//!
//! The dialog is a modal like the studio dialogs and speaks their
//! [`Message`](DialogMessage)s: Cancel is "Later" (and a barrier tap or
//! Escape), Confirm its primary button, Secondary the button beside it. A
//! screen shows it through the app's dialog stack (`Update::dialog`,
//! `Dialog::UpdateDownload`), so it covers the whole window as Flutter's
//! dialog route did. It starts and cancels the update on the shared
//! self-update itself (neither blocks); opening the browser is an effect it
//! returns ([`DialogOutcome::OpenUrl`]), which the app runs
//! ([`open_url_task`]), and the dialog stays open on purpose then: if the
//! browser launch fails, the release URL it shows is the fallback,
//! selectable for copying by hand (`text_field::TextEditor::read_only`: it
//! wraps at words, a word longer than the line broken between its letters,
//! selects and copies, and shows no caret).

use std::sync::Arc;

use crate::ui::core::text;
use iced::alignment::Vertical;
use iced::widget::text::Wrapping;
use iced::widget::{column, container, row, Space};
use iced::{Background, Border, Color, Element, Length, Padding, Pixels, Task, Theme};
use vocara_services::self_update::{Phase, SelfInstall, SelfUpdate};
use vocara_services::update_check_service::{UpdateCheckService, UpdateInfo};

use crate::ui::core::icons::{self, Icon};
use crate::ui::core::theme::{self, alpha, font, type_scale, Studio};
use crate::ui::core::widgets::confirm_dialog::{self, modal, Fade};
use crate::ui::core::widgets::controls::{
    dialog_button, icon_button, secondary_button, DialogButton, SLIDER_FILL,
};
use crate::ui::core::widgets::fading_scrollbar::{scrollable, Fading};
use crate::ui::core::widgets::studio_button::{studio_button, StudioButtonVariant};
use crate::ui::core::widgets::studio_hint::spinner;
use crate::ui::core::widgets::studio_text::studio_text;
use crate::ui::core::widgets::text_field::editor::{self as text_editor, text_editor};

pub use crate::ui::core::widgets::confirm_dialog::Message as DialogMessage;

/// The program that opens a URL in the default browser: `/usr/bin/open` on
/// macOS (the same pattern main uses to reveal the library folder in Finder;
/// no url_launcher dependency), `xdg-open` on the Linux development build.
#[cfg(target_os = "macos")]
const OPENER: &str = "open";
#[cfg(not(target_os = "macos"))]
const OPENER: &str = "xdg-open";

/// Opens `url` in the default browser. Returns false when the launch fails —
/// the dialog keeps the selectable URL on screen as the fallback. Blocking:
/// run it off the UI thread ([`open_url_task`]).
pub fn open_url_in_browser(url: &str) -> bool {
    std::process::Command::new(OPENER)
        .arg(url)
        .status()
        .is_ok_and(|status| status.success())
}

/// [`open_url_in_browser`] on a blocking thread, its outcome dropped (Dart
/// ran it `unawaited`).
pub fn open_url_task<M: Send + 'static>(url: String) -> Task<M> {
    Task::future(crate::util::blocking(move || open_url_in_browser(&url))).discard()
}

/// The progress bar's width in the banner.
pub const BANNER_BAR_WIDTH: f32 = 160.0;

/// A progress bar's height (`DESIGN.md`'s thin bars: the metric cards'
/// 4 px).
pub const BAR_HEIGHT: f32 = 4.0;

/// `bytes` as Finder writes a download's size: kB under a megabyte, then MB
/// with one decimal (decimal units).
pub fn human_size(bytes: u64) -> String {
    if bytes < 1_000_000 {
        format!("{} kB", bytes.div_ceil(1000))
    } else {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    }
}

/// The download's progress as a line: "12.3 MB of 40.1 MB".
pub fn progress_line(done: u64, total: u64) -> String {
    format!("{} of {}", human_size(done), human_size(total))
}

/// The version an update under way installs, else `offered`'s.
fn updating_version(updater: &SelfUpdate, offered: Option<&UpdateInfo>) -> String {
    updater
        .version()
        .or_else(|| offered.map(|o| o.version.clone()))
        .unwrap_or_default()
}

/// What the banner and Settings say while an update installs.
pub fn installing_line(version: &str) -> String {
    format!("Installing Vocara {version}… Vocara will relaunch.")
}

/// What they say while the downloaded update waits for work the relaunch
/// would cut short — a take being recorded or saved, an import, a
/// separation (installing quits Vocara for the relaunch).
pub fn waiting_line(version: &str) -> String {
    format!("Vocara {version} is ready. It installs once your work is saved.")
}

/// What they say while it downloads, before its progress.
pub fn downloading_line(version: &str) -> String {
    format!("Downloading Vocara {version}…")
}

/// What they say when it failed.
pub fn failed_line(reason: &str) -> String {
    format!("Update failed: {reason}")
}

/// What the dialog says when this copy cannot replace itself.
pub fn blocked_line(reason: &str) -> String {
    format!("Vocara can't update itself here: {reason}.")
}

// -------------------------------------------------------------------- banner

/// What the banner reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// "Update now" (and "Try again"): start the self-update of this
    /// release.
    UpdateNow(UpdateInfo),
    /// "What's new", "Download" or "Details": open the dialog for this
    /// release.
    Open(UpdateInfo),
    /// The download's Cancel.
    CancelDownload,
    /// The dismiss cross: the offer for the rest of the run, a failure for
    /// good.
    Dismiss,
}

/// `UpdateNoticeBanner(updates: ...)`, with the self-update's states (see
/// the module docs): nothing until there is something to say — an offer
/// not dismissed this run, an update under way, or a failure — and the
/// offer goes for the rest of the run once dismissed (Settings still shows
/// it).
pub fn update_notice_banner<'a>(
    updates: &UpdateCheckService,
    updater: &SelfUpdate,
) -> Element<'a, Message> {
    let offer = updates.available();
    match updater.phase() {
        Phase::Downloading { done, total } => {
            downloading_banner(&updating_version(updater, offer.as_ref()), done, total)
        }
        Phase::WaitingForWork => {
            progress_banner(waiting_line(&updating_version(updater, offer.as_ref())))
        }
        Phase::Installing | Phase::Relaunching => {
            progress_banner(installing_line(&updating_version(updater, offer.as_ref())))
        }
        Phase::Failed(reason) => failed_banner(reason, offer, updater),
        Phase::Idle => match offer {
            Some(info) if !updates.notice_dismissed() => {
                let blocked = matches!(updater.self_install(&info), SelfInstall::Blocked(_));
                offer_banner(info, blocked)
            }
            _ => Space::new().into(),
        },
    }
}

/// Handles a banner message: "Update now" starts the self-update, Cancel
/// stops its download, Dismiss puts the offer (and a failure) away; the
/// rest return the dialog to show.
pub fn update(
    updates: &UpdateCheckService,
    updater: &Arc<SelfUpdate>,
    message: Message,
) -> Option<UpdateDownloadDialog> {
    match message {
        Message::UpdateNow(info) => {
            updater.start(&info);
            None
        }
        Message::Open(info) => Some(UpdateDownloadDialog::new(info, Arc::clone(updater))),
        Message::CancelDownload => {
            updater.cancel();
            None
        }
        Message::Dismiss => {
            updater.dismiss_failure();
            updates.dismiss_notice();
            None
        }
    }
}

/// The banner's frame: `content` on the accent at 8 % over the library's
/// 32 px gutters, a hairline under it.
fn frame<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(column![
        container(content)
            .width(Length::Fill)
            .padding(Padding::from([8, 32])),
        container(Space::new())
            .width(Length::Fill)
            .height(1)
            .style(theme::fill(Studio::BORDER)),
    ])
    .width(Length::Fill)
    .style(|_: &Theme| theme::boxed(Some(alpha(Studio::ACCENT, 0.08)), 0.0, None))
    .into()
}

/// The banner's line: 12 px medium `TEXT_PRIMARY`, on one line (or `lines`),
/// ellipsized.
fn banner_line<'a>(line: String, lines: usize) -> Element<'a, Message> {
    studio_text(line)
        .size(type_scale::BODY)
        .line_height(Pixels(type_scale::LINE))
        .font(font::MEDIUM)
        .color(Studio::TEXT_PRIMARY)
        .width(Length::Fill)
        .ellipsis(lines)
        .into()
}

/// The offer: the accent's arrow and the news, then "What's new" and the
/// primary "Update now" — or, where Vocara cannot replace itself, the
/// secondary "Download" that opens the dialog (which says why) — and the
/// dismiss icon button.
fn offer_banner<'a>(info: UpdateInfo, blocked: bool) -> Element<'a, Message> {
    let mut content = row![
        icons::icon(Icon::CircleArrowUp, 14.0, Studio::ACCENT_BRIGHT),
        Space::new().width(10),
        banner_line(format!("Vocara {} is available.", info.version), 1),
        Space::new().width(10),
    ]
    .align_y(Vertical::Center);
    content = if blocked {
        content.push(studio_button("Download", Message::Open(info)).icon(Icon::Download))
    } else {
        content
            .push(studio_button("What's new", Message::Open(info.clone())))
            .push(Space::new().width(8))
            .push(
                studio_button("Update now", Message::UpdateNow(info))
                    .icon(Icon::Download)
                    .variant(StudioButtonVariant::Accent),
            )
    };
    frame(content.push(Space::new().width(6)).push(dismiss_button()))
}

/// Downloading: what and how far (tabular, so the count does not jitter),
/// the bar, and Cancel.
fn downloading_banner<'a>(version: &str, done: u64, total: u64) -> Element<'a, Message> {
    frame(
        row![
            icons::icon(Icon::Download, 14.0, Studio::ACCENT_BRIGHT),
            Space::new().width(10),
            banner_line(downloading_line(version), 1),
            Space::new().width(10),
            studio_text(progress_line(done, total))
                .size(type_scale::LABEL)
                .line_height(Pixels(type_scale::LINE))
                .tabular()
                .color(Studio::TEXT_SECONDARY),
            Space::new().width(10),
            progress_bar(done, total, Length::Fixed(BANNER_BAR_WIDTH)),
            Space::new().width(12),
            studio_button("Cancel", Message::CancelDownload),
        ]
        .align_y(Vertical::Center),
    )
}

/// Installing: a spinner and the line; nothing to press (the app quits for
/// the relaunch on its own).
fn progress_banner<'a>(line: String) -> Element<'a, Message> {
    frame(
        container(
            row![
                spinner(12.0, 1.6, Studio::TEXT_SECONDARY),
                Space::new().width(10),
                banner_line(line, 1),
            ]
            .align_y(Vertical::Center),
        )
        // As tall as a banner with buttons.
        .height(theme::height::TOOLBAR_BUTTON)
        .align_y(Vertical::Center),
    )
}

/// A failure: the warning and why (on up to two lines), then "Details" and
/// "Try again" when the release is still on offer and Vocara can replace
/// itself, and the dismiss icon button.
fn failed_banner<'a>(
    reason: String,
    offer: Option<UpdateInfo>,
    updater: &SelfUpdate,
) -> Element<'a, Message> {
    let mut content = row![
        icons::icon(Icon::TriangleAlert, 14.0, Studio::WARN_TEXT),
        Space::new().width(10),
        banner_line(failed_line(&reason), 2),
        Space::new().width(10),
    ]
    .align_y(Vertical::Center);
    if let Some(info) = offer {
        content = content.push(studio_button("Details", Message::Open(info.clone())));
        if !matches!(updater.self_install(&info), SelfInstall::Blocked(_)) {
            content = content.push(Space::new().width(8)).push(
                studio_button("Try again", Message::UpdateNow(info))
                    .icon(Icon::RefreshCw)
                    .variant(StudioButtonVariant::Accent),
            );
        }
    }
    frame(content.push(Space::new().width(6)).push(dismiss_button()))
}

/// A 28 px icon button with a 14 px cross and a "Dismiss" tooltip.
fn dismiss_button<'a>() -> Element<'a, Message> {
    icon_button(Icon::X, Message::Dismiss)
        .icon_size(14.0)
        .tooltip("Dismiss")
        .into()
}

/// A download's progress: a 4 px bar, radius 2, its track `CONTROL_ACTIVE`
/// and its fill the neutral slider fill (`DESIGN.md`: one accent, used for
/// meaning; progress is not a selection).
pub fn progress_bar<'a, M: 'a>(done: u64, total: u64, width: Length) -> Element<'a, M> {
    // Nothing to show of a download of no known size.
    let permille = (done.min(total) * 1000).checked_div(total).unwrap_or(0) as u16;
    let fill = container(Space::new())
        .width(Length::FillPortion(permille.max(1)))
        .height(Length::Fill)
        .style(|_: &Theme| theme::boxed(Some(SLIDER_FILL), BAR_HEIGHT / 2.0, None));
    let bar: Element<'a, M> = if permille == 0 {
        Space::new().width(Length::Fill).into()
    } else if permille >= 1000 {
        fill.into()
    } else {
        row![
            fill,
            Space::new().width(Length::FillPortion(1000 - permille))
        ]
        .into()
    };
    container(bar)
        .width(width)
        .height(BAR_HEIGHT)
        .style(|_: &Theme| theme::boxed(Some(Studio::CONTROL_ACTIVE), BAR_HEIGHT / 2.0, None))
        .into()
}

// -------------------------------------------------------------------- dialog

/// How a [`DialogMessage`] left the update dialog, when it asks the app for
/// something.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogOutcome {
    /// "Later", a barrier tap or Escape: close it.
    Close,
    /// "Open Download Page": open this URL in the browser, and stay open.
    OpenUrl(String),
}

/// What the dialog shows below the notes, from the self-update's phase and
/// whether this copy can replace itself.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Mode {
    /// "Update now".
    Offer,
    /// It cannot update itself, and why: the release page.
    Fallback(String),
    Downloading {
        done: u64,
        total: u64,
    },
    /// Downloaded, waiting for a take to be saved: Cancel calls it off.
    Waiting,
    Installing,
    Failed(String),
}

/// The update dialog for a newer release (`showUpdateDownloadDialog`):
/// version, release notes, and what can be done about it — "Update now",
/// the download's progress with Cancel, the install, a failure with "Try
/// again" — or, where Vocara cannot replace itself, why, and the release
/// page URL as selectable text (so it stays reachable even when opening the
/// browser fails) with an "Open Download Page" button.
#[derive(Clone)]
pub struct UpdateDownloadDialog {
    info: UpdateInfo,
    /// The URL as the selectable text holds it, with its selection.
    url: text_editor::Content,
    /// The shared self-update the dialog starts, cancels and draws.
    updater: Arc<SelfUpdate>,
}

impl std::fmt::Debug for UpdateDownloadDialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpdateDownloadDialog")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

/// Two dialogs are the same when they offer the same release (the URL's
/// selection is the text's own state).
impl PartialEq for UpdateDownloadDialog {
    fn eq(&self, other: &Self) -> bool {
        self.info == other.info && self.url.text() == other.url.text()
    }
}

impl Eq for UpdateDownloadDialog {}

impl UpdateDownloadDialog {
    pub fn new(info: UpdateInfo, updater: Arc<SelfUpdate>) -> Self {
        let url = text_editor::Content::with_text(&info.url);
        Self { info, url, updater }
    }

    /// The release it offers.
    pub fn info(&self) -> &UpdateInfo {
        &self.info
    }

    fn mode(&self) -> Mode {
        match self.updater.phase() {
            Phase::Downloading { done, total } => Mode::Downloading { done, total },
            Phase::WaitingForWork => Mode::Waiting,
            Phase::Installing | Phase::Relaunching => Mode::Installing,
            phase => match self.updater.self_install(&self.info) {
                SelfInstall::Blocked(reason) => Mode::Fallback(reason),
                _ => match phase {
                    Phase::Failed(reason) => Mode::Failed(reason),
                    _ => Mode::Offer,
                },
            },
        }
    }

    /// Handles `message`; `None` while the dialog just stays (having started
    /// or cancelled the update, perhaps).
    pub fn update(&mut self, message: DialogMessage) -> Option<DialogOutcome> {
        let mode = self.mode();
        match message {
            DialogMessage::Cancel | DialogMessage::Decline => Some(DialogOutcome::Close),
            DialogMessage::Confirm => match mode {
                // The dialog stays open on purpose: if the browser launch
                // fails the selectable URL is the fallback.
                Mode::Fallback(_) => Some(DialogOutcome::OpenUrl(self.info.url.clone())),
                Mode::Offer | Mode::Failed(_) => {
                    self.updater.start(&self.info);
                    None
                }
                Mode::Downloading { .. } | Mode::Waiting | Mode::Installing => None,
            },
            DialogMessage::Secondary => match mode {
                Mode::Downloading { .. } | Mode::Waiting => {
                    self.updater.cancel();
                    None
                }
                Mode::Failed(_) => Some(DialogOutcome::OpenUrl(self.info.url.clone())),
                Mode::Offer | Mode::Fallback(_) | Mode::Installing => None,
            },
            // The URL is read-only: it sends nothing, and an edit to it
            // would be dropped.
            DialogMessage::Input(_) => None,
        }
    }

    /// The dialog over the whole window, all in: the barrier and the
    /// centred card (480 wide, the studio dialog card).
    pub fn view(&self) -> Element<'_, DialogMessage> {
        self.view_at(Fade::SHOWN)
    }

    /// The dialog at `fade` (`confirm_dialog`'s module docs).
    pub fn view_at(&self, fade: Fade) -> Element<'_, DialogMessage> {
        let info = &self.info;
        let mut content = column![text(format!("Vocara {} is available", info.version))
            .size(type_scale::TITLE)
            .line_height(iced::Pixels(18.0))
            .font(font::SEMIBOLD)
            .color(Studio::TEXT_PRIMARY)];
        if !info.notes.is_empty() {
            content = content.push(Space::new().height(8)).push(
                container(
                    scrollable(
                        text(info.notes.as_str())
                            .size(type_scale::BODY)
                            .line_height(iced::Pixels(18.0))
                            .color(Studio::TEXT_SECONDARY)
                            .width(Length::Fill),
                    )
                    .fading(),
                )
                .max_height(200),
            );
        }
        let later = "Later";
        let buttons = match self.mode() {
            Mode::Offer => buttons(later, None, ("Update now", Some(DialogMessage::Confirm))),
            Mode::Fallback(reason) => {
                content = content
                    .push(Space::new().height(12))
                    .push(note(blocked_line(&reason)))
                    .push(Space::new().height(12))
                    .push(self.url_well());
                buttons(
                    later,
                    None,
                    ("Open Download Page", Some(DialogMessage::Confirm)),
                )
            }
            Mode::Downloading { done, total } => {
                content = content
                    .push(Space::new().height(14))
                    .push(downloading_block(
                        &updating_version(&self.updater, Some(info)),
                        done,
                        total,
                    ));
                buttons(later, None, ("Update now", None))
            }
            Mode::Waiting => {
                content = content
                    .push(Space::new().height(14))
                    .push(note(waiting_line(&updating_version(
                        &self.updater,
                        Some(info),
                    ))));
                buttons(later, Some("Cancel update"), ("Update now", None))
            }
            Mode::Installing => {
                content = content.push(Space::new().height(14)).push(
                    row![
                        spinner(12.0, 1.6, Studio::TEXT_SECONDARY),
                        Space::new().width(8),
                        note(installing_line(&updating_version(
                            &self.updater,
                            Some(info)
                        ))),
                    ]
                    .align_y(Vertical::Center),
                );
                buttons(later, None, ("Update now", None))
            }
            Mode::Failed(reason) => {
                content = content.push(Space::new().height(12)).push(
                    row![
                        // Centred on the line's first 18 px.
                        container(icons::icon(Icon::TriangleAlert, 14.0, Studio::WARN_TEXT))
                            .height(18)
                            .align_y(Vertical::Center),
                        Space::new().width(8),
                        note(failed_line(&reason)),
                    ]
                    .align_y(Vertical::Top),
                );
                buttons(
                    later,
                    Some("Open Download Page"),
                    ("Try again", Some(DialogMessage::Confirm)),
                )
            }
        };
        content = content.push(Space::new().height(20)).push(buttons);
        let card = container(content.width(Length::Fill))
            .width(DIALOG_WIDTH)
            .padding(confirm_dialog::CARD_PADDING)
            .style(theme::dialog_card);
        modal(container(card).padding(24).into(), fade)
    }

    /// The release page's URL, selectable so the user can copy the address
    /// by hand if the browser cannot be opened (`SelectableText`: no line
    /// limit, no caret), in the text field's well, its text 10 in from its
    /// sides and 8 from its top and bottom.
    fn url_well(&self) -> Element<'_, DialogMessage> {
        let url = text_editor(&self.url)
            .read_only(true)
            .size(12)
            .line_height(1.0)
            .padding(0)
            .wrapping(Wrapping::WordOrGlyph)
            .style(url_style);
        container(url)
            .width(Length::Fill)
            .padding(Padding::from([8, 10]))
            .style(theme::field_well(false))
            .into()
    }
}

/// The dialog's width.
pub const DIALOG_WIDTH: f32 = 480.0;

/// A line of the dialog's state: 12 px `TEXT_SECONDARY` on 18 px lines,
/// wrapping.
fn note<'a>(line: String) -> Element<'a, DialogMessage> {
    text(line)
        .size(type_scale::BODY)
        .line_height(Pixels(18.0))
        .color(Studio::TEXT_SECONDARY)
        .width(Length::Fill)
        .into()
}

/// The download in the dialog: what and how far over the full-width bar,
/// and its Cancel (the dialog's second action) at the right.
fn downloading_block<'a>(version: &str, done: u64, total: u64) -> Element<'a, DialogMessage> {
    let labels = row![
        studio_text(downloading_line(version))
            .size(type_scale::BODY)
            .line_height(Pixels(18.0))
            .color(Studio::TEXT_SECONDARY)
            .width(Length::Fill),
        studio_text(progress_line(done, total))
            .size(type_scale::LABEL)
            .line_height(Pixels(18.0))
            .tabular()
            .color(Studio::TEXT_FAINT),
    ]
    .align_y(Vertical::Center);
    row![
        column![
            labels,
            Space::new().height(6),
            progress_bar(done, total, Length::Fill)
        ]
        .width(Length::Fill),
        Space::new().width(12),
        secondary_button("Cancel", Some(DialogMessage::Secondary)),
    ]
    .align_y(Vertical::Center)
    .into()
}

/// The buttons, right-aligned (`DESIGN.md`, dialogs): the secondary ones
/// (Later, which closes; then the second action, if any), then the primary
/// one (disabled with no message), 8 apart.
fn buttons<'a>(
    later: &'a str,
    second: Option<&'a str>,
    (primary, on_primary): (&'a str, Option<DialogMessage>),
) -> Element<'a, DialogMessage> {
    let mut buttons = row![
        Space::new().width(Length::Fill),
        secondary_button(later, Some(DialogMessage::Cancel)),
    ]
    .spacing(8);
    if let Some(second) = second {
        buttons = buttons.push(secondary_button(second, Some(DialogMessage::Secondary)));
    }
    buttons
        .push(dialog_button(primary, on_primary, DialogButton::Primary))
        .into()
}

/// The URL: `Studio.textSecondary`, 12 px, on the field's well, with the
/// fields' selection colour.
fn url_style(_: &Theme, _: text_editor::Status) -> text_editor::Style {
    text_editor::Style {
        background: Background::Color(Color::TRANSPARENT),
        border: Border::default(),
        placeholder: Studio::TEXT_FAINT,
        value: Studio::TEXT_SECONDARY,
        selection: crate::ui::core::theme::SELECTION,
    }
}

#[cfg(test)]
pub(crate) mod testing;

#[cfg(test)]
mod tests;
