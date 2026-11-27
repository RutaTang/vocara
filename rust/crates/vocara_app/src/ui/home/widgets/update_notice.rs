//! Port of `ui/home/widgets/update_notice.dart`: the dismissible "a newer
//! Vocara exists" banner under the library's toolbar ([`update_notice_banner`]),
//! the download dialog for a newer release ([`UpdateDownloadDialog`], which
//! Settings opens too), and [`open_url_in_browser`].
//!
//! Dart's banner listened to the `UpdateCheckService` itself; here the
//! screen rebuilds its view when the service notifies, and the banner reads
//! the service as it is then. Its messages go to [`update`], which dismisses
//! the notice on the service or hands back the dialog to show.
//!
//! The dialog is a modal like the studio dialogs and speaks their
//! [`Message`](DialogMessage)s: Cancel is "Later" (and a barrier tap or
//! Escape), Confirm is "Open Download Page". A screen shows it through the
//! app's dialog stack (`Update::dialog`, `Dialog::UpdateDownload`), so it
//! covers the whole window as Flutter's dialog route did. Opening the
//! browser is an effect the dialog returns ([`DialogOutcome::OpenUrl`]),
//! which the app runs ([`open_url_task`]) — the counterpart of Dart's
//! `openUrl` test seam. The dialog stays open on purpose: if the browser
//! launch fails, the release URL it shows is the fallback, selectable for
//! copying by hand.
//! Flutter's `SelectableText` is the read-only multi-line field here
//! (`text_field::TextEditor::read_only`): the address wraps as the text did
//! (no line limit, at words, a word longer than the line broken between
//! its letters), selects and copies (the right-click menu offering Copy),
//! and shows no caret; its selection is its own, so it sends the dialog
//! nothing.

use crate::ui::core::text;
use iced::alignment::Vertical;
use iced::widget::text::Wrapping;
use iced::widget::{column, container, row, Space};
use iced::{Background, Border, Color, Element, Length, Padding, Task, Theme};
use vocara_services::update_check_service::{UpdateCheckService, UpdateInfo};

use crate::ui::core::icons::{self, Icon};
use crate::ui::core::theme::{self, alpha, font, type_scale, Studio};
use crate::ui::core::widgets::confirm_dialog::{self, modal, Fade};
use crate::ui::core::widgets::controls::{icon_button, DialogButton};
use crate::ui::core::widgets::fading_scrollbar::{scrollable, Fading};
use crate::ui::core::widgets::studio_button::studio_button;
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

// -------------------------------------------------------------------- banner

/// What the banner reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// "Download": open the dialog for this release.
    Download(UpdateInfo),
    /// The dismiss cross.
    Dismiss,
}

/// `UpdateNoticeBanner(updates: ...)`: renders nothing until
/// [`UpdateCheckService::available`] is set, and disappears for the rest of
/// the run once dismissed (Settings still shows the update).
pub fn update_notice_banner<'a>(updates: &UpdateCheckService) -> Element<'a, Message> {
    match updates.available() {
        Some(info) if !updates.notice_dismissed() => banner(info),
        _ => Space::new().into(),
    }
}

/// Handles a banner message: Dismiss hides the notice for the rest of the
/// run; Download returns the dialog to show.
pub fn update(updates: &UpdateCheckService, message: Message) -> Option<UpdateDownloadDialog> {
    match message {
        Message::Download(info) => Some(UpdateDownloadDialog::new(info)),
        Message::Dismiss => {
            updates.dismiss_notice();
            None
        }
    }
}

/// The banner: the accent's arrow and the news (12 px medium), the
/// secondary "Download" button and the dismiss icon button, on the accent
/// at 8 % over the library's 32 px gutters, a hairline under it.
fn banner<'a>(info: UpdateInfo) -> Element<'a, Message> {
    let content = row![
        icons::icon(Icon::CircleArrowUp, 14.0, Studio::ACCENT_BRIGHT),
        Space::new().width(10),
        // One line, ellipsized.
        studio_text(format!("Vocara {} is available.", info.version))
            .size(type_scale::BODY)
            .line_height(iced::Pixels(type_scale::LINE))
            .font(font::MEDIUM)
            .color(Studio::TEXT_PRIMARY)
            .width(Length::Fill)
            .ellipsis(1),
        Space::new().width(10),
        studio_button("Download", Message::Download(info)).icon(Icon::Download),
        Space::new().width(6),
        dismiss_button(),
    ]
    .align_y(Vertical::Center);
    // The accent at 8 %, with a hairline under the padding.
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

/// A 28 px icon button with a 14 px cross and a "Dismiss" tooltip.
fn dismiss_button<'a>() -> Element<'a, Message> {
    icon_button(Icon::X, Message::Dismiss)
        .icon_size(14.0)
        .tooltip("Dismiss")
        .into()
}

// -------------------------------------------------------------------- dialog

/// How a [`DialogMessage`] left the download dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogOutcome {
    /// "Later", a barrier tap or Escape: close it.
    Close,
    /// "Open Download Page": open this URL in the browser, and stay open.
    OpenUrl(String),
}

/// The download dialog for a newer release (`showUpdateDownloadDialog`):
/// version, release notes, and the release page URL as selectable text (so
/// it stays reachable even when opening the browser fails), plus an "Open
/// Download Page" button.
#[derive(Debug, Clone)]
pub struct UpdateDownloadDialog {
    info: UpdateInfo,
    /// The URL as the selectable text holds it, with its selection.
    url: text_editor::Content,
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
    pub fn new(info: UpdateInfo) -> Self {
        let url = text_editor::Content::with_text(&info.url);
        Self { info, url }
    }

    /// The release it offers.
    pub fn info(&self) -> &UpdateInfo {
        &self.info
    }

    /// Handles `message`; `None` while the dialog just stays as it is.
    pub fn update(&mut self, message: DialogMessage) -> Option<DialogOutcome> {
        match message {
            DialogMessage::Cancel => Some(DialogOutcome::Close),
            // The dialog stays open on purpose: if the browser launch fails
            // the selectable URL is the fallback.
            DialogMessage::Confirm => Some(DialogOutcome::OpenUrl(self.info.url.clone())),
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
        // Selectable so the user can copy the address by hand if the browser
        // cannot be opened (`SelectableText`: no line limit, no caret).
        let url = text_editor(&self.url)
            .read_only(true)
            .size(12)
            .line_height(1.0)
            .padding(0)
            .wrapping(Wrapping::WordOrGlyph)
            .style(url_style);
        content = content
            .push(Space::new().height(14))
            .push(
                // The text field's well, its text 10 in from its sides and
                // 8 from its top and bottom.
                container(url)
                    .width(Length::Fill)
                    .padding(Padding::from([8, 10]))
                    .style(theme::field_well(false)),
            )
            .push(Space::new().height(20))
            .push(confirm_dialog::buttons(
                "Later",
                DialogMessage::Cancel,
                "Open Download Page",
                Some(DialogMessage::Confirm),
                DialogButton::Primary,
            ));
        let card = container(content.width(Length::Fill))
            .width(DIALOG_WIDTH)
            .padding(confirm_dialog::CARD_PADDING)
            .style(theme::dialog_card);
        modal(container(card).padding(24).into(), fade)
    }
}

/// The dialog's width.
pub const DIALOG_WIDTH: f32 = 480.0;

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
mod tests {
    use super::*;
    use iced_test::selector::Candidate;
    use vocara_services::update_check_service::{FetchError, UpdateStatus};

    /// A service whose manifest advertises 9.9.9 over a running 1.0.0.
    fn newer_available() -> UpdateCheckService {
        UpdateCheckService::with_fetcher(
            Box::new(|_| {
                Ok(r#"{"latest": "9.9.9", "url": "https://example.com/download", "notes": "New voiceprint views."}"#.to_string())
            }),
            "1.0.0",
        )
    }

    fn banner_messages(
        updates: &UpdateCheckService,
        drive: impl FnOnce(&mut iced_test::Simulator<'_, Message>),
    ) -> Vec<Message> {
        let mut ui = iced_test::simulator(update_notice_banner(updates));
        drive(&mut ui);
        ui.into_messages().collect()
    }

    fn shows(updates: &UpdateCheckService, label: &str) -> bool {
        let mut ui = iced_test::simulator(update_notice_banner(updates));
        ui.find(label).is_ok()
    }

    #[test]
    fn a_newer_release_raises_the_banner() {
        let updates = newer_available();
        assert!(!shows(&updates, "Download"), "nothing before a check");
        updates.check();
        assert!(shows(&updates, "Vocara 9.9.9 is available."));
        assert!(shows(&updates, "Download"));
    }

    #[test]
    fn an_up_to_date_check_shows_no_banner() {
        let updates = UpdateCheckService::with_fetcher(
            Box::new(|_| Ok(r#"{"latest": "1.0.0", "url": "https://example.com"}"#.to_string())),
            "1.0.0",
        );
        updates.check();
        assert_eq!(updates.status(), UpdateStatus::UpToDate);
        assert!(!shows(&updates, "Download"));
    }

    #[test]
    fn a_failed_check_stays_silent() {
        let updates = UpdateCheckService::with_fetcher(
            Box::new(|_| Err(FetchError("offline".into()))),
            "1.0.0",
        );
        updates.check();
        assert_eq!(updates.status(), UpdateStatus::Unknown);
        assert!(!shows(&updates, "Download"));
    }

    /// The dialog the banner's Download button opens.
    fn open_dialog(updates: &UpdateCheckService) -> UpdateDownloadDialog {
        let messages = banner_messages(updates, |ui| {
            ui.click("Download").expect("the download button");
        });
        assert_eq!(messages.len(), 1);
        let message = messages.into_iter().next().expect("a message");
        update(updates, message).expect("the dialog opens")
    }

    fn dialog_messages(
        dialog: &UpdateDownloadDialog,
        drive: impl FnOnce(&mut iced_test::Simulator<'_, DialogMessage>),
    ) -> Vec<DialogMessage> {
        let mut ui = iced_test::simulator(dialog.view());
        drive(&mut ui);
        ui.into_messages().collect()
    }

    #[test]
    fn download_opens_the_dialog_with_notes_and_a_selectable_url() {
        let updates = newer_available();
        updates.check();
        let mut dialog = open_dialog(&updates);
        let messages = dialog_messages(&dialog, |ui| {
            assert!(ui.find("Vocara 9.9.9 is available").is_ok());
            assert!(ui.find("New voiceprint views.").is_ok());
            // The URL is selectable text: text, not an editable field.
            assert!(ui.find("https://example.com/download").is_ok());
            let field = ui.find(|candidate: Candidate<'_>| match candidate {
                Candidate::TextInput { .. } => Some(()),
                _ => None,
            });
            assert!(field.is_err(), "no editable field");
            ui.click("Later").expect("the later button");
        });
        assert_eq!(messages, vec![DialogMessage::Cancel]);
        assert_eq!(
            dialog.update(DialogMessage::Cancel),
            Some(DialogOutcome::Close)
        );
    }

    #[test]
    fn the_url_wraps_in_its_well_inside_the_card() {
        // The 480-wide card centred in the 1024-wide window, 20 inside its
        // inset outline; the well's text 10 in from its sides and 8 down;
        // the text at its 12 px lines, as many as it takes.
        use crate::ui::core::widgets::confirm_dialog::CARD_PADDING;
        crate::ui::core::widgets::studio_text::testing::load_fonts();
        let card_left = (1024.0 - DIALOG_WIDTH) / 2.0;
        let width = DIALOG_WIDTH - 2.0 * CARD_PADDING - 2.0 * 10.0;
        let dialog = |url: &str| {
            UpdateDownloadDialog::new(UpdateInfo {
                version: "2.0.0".into(),
                url: url.into(),
                notes: String::new(),
            })
        };
        let short = dialog("https://example.com/2");
        let mut ui = iced_test::simulator(short.view());
        let title = ui.find("Vocara 2.0.0 is available").expect("the title");
        let url = ui.find("https://example.com/2").expect("the URL").bounds();
        assert_eq!(title.bounds().x, card_left + CARD_PADDING);
        assert_eq!(
            (url.x, url.width, url.height),
            (card_left + CARD_PADDING + 10.0, width, 12.0)
        );
        // The title (an 18 px line), 14, the well's padding.
        assert_eq!(url.y, title.bounds().y + 18.0 + 14.0 + 8.0);

        let long = format!("https://example.com/releases/{}", "a".repeat(120));
        let long_dialog = dialog(&long);
        let mut ui = iced_test::simulator(long_dialog.view());
        let url = ui.find(long.as_str()).expect("the URL").bounds();
        assert!(url.height >= 24.0 && url.height % 12.0 == 0.0, "{url:?}");
    }

    #[test]
    fn the_url_cannot_be_edited() {
        let updates = newer_available();
        updates.check();
        let mut dialog = open_dialog(&updates);
        let before = dialog.clone();
        assert_eq!(dialog.update(DialogMessage::Input("x".into())), None);
        assert_eq!(dialog, before);
    }

    #[test]
    fn dismissing_the_banner_hides_it_for_the_rest_of_the_run() {
        let updates = newer_available();
        updates.check();
        assert!(shows(&updates, "Vocara 9.9.9 is available."));
        let cross = char::from(Icon::X).to_string();
        let messages = banner_messages(&updates, |ui| {
            ui.click(cross.as_str()).expect("the dismiss button");
        });
        assert_eq!(messages, vec![Message::Dismiss]);
        for m in messages {
            assert!(update(&updates, m).is_none());
        }
        assert!(!shows(&updates, "Vocara 9.9.9 is available."));
        assert!(updates.notice_dismissed());
        // A later check that finds it again keeps it dismissed.
        updates.check();
        assert!(!shows(&updates, "Download"));
    }

    #[test]
    fn open_download_page_opens_the_release_url_and_keeps_the_dialog() {
        let updates = newer_available();
        updates.check();
        let mut dialog = open_dialog(&updates);
        let messages = dialog_messages(&dialog, |ui| {
            ui.click("Open Download Page").expect("the open button");
        });
        let outcomes: Vec<_> = messages
            .into_iter()
            .filter_map(|m| dialog.update(m))
            .collect();
        assert_eq!(
            outcomes,
            vec![DialogOutcome::OpenUrl(
                "https://example.com/download".into()
            )]
        );
    }

    #[test]
    fn a_release_without_notes_offers_its_url_all_the_same() {
        // Settings opens the dialog for the update it shows.
        let mut dialog = UpdateDownloadDialog::new(UpdateInfo {
            version: "2.0.0".into(),
            url: "https://example.com/2".into(),
            notes: String::new(),
        });
        let messages = dialog_messages(&dialog, |ui| {
            assert!(ui.find("Vocara 2.0.0 is available").is_ok());
            ui.click("Open Download Page").expect("the open button");
            let _ = ui.tap_key(iced::keyboard::Key::Named(
                iced::keyboard::key::Named::Escape,
            ));
        });
        let outcomes: Vec<_> = messages
            .into_iter()
            .filter_map(|m| dialog.update(m))
            .collect();
        assert_eq!(
            outcomes,
            vec![
                DialogOutcome::OpenUrl("https://example.com/2".into()),
                DialogOutcome::Close
            ]
        );
    }
}
