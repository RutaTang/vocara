//! Port of `ui/home/widgets/update_notice.dart`: the dismissible "a newer
//! Vocara exists" banner under the home header ([`update_notice_banner`]),
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
//! Flutter's `SelectableText` is a read-only text field here: it selects and
//! copies, and its edits are dropped (the dialog keeps the URL).

use iced::alignment::Vertical;
use iced::widget::{button, column, container, row, scrollable, text, text_input, tooltip, Space};
use iced::{Background, Border, Color, Element, Length, Padding, Task, Theme};
use vocara_services::update_check_service::{UpdateCheckService, UpdateInfo};

use crate::ui::core::icons::{self, Icon};
use crate::ui::core::theme::{self, alpha, bare_button, font, Studio, Zinc};
use crate::ui::core::widgets::confirm_dialog::modal;
use crate::ui::core::widgets::studio_button::{studio_button, StudioButtonVariant};
use crate::ui::core::widgets::studio_text::studio_text;

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

fn banner<'a>(info: UpdateInfo) -> Element<'a, Message> {
    let content = row![
        icons::icon(Icon::CircleArrowUp, 16.0, Studio::ACCENT),
        Space::new().width(10),
        // One line, ellipsized.
        studio_text(format!("Vocara {} is available.", info.version))
            .size(13)
            .font(font::SEMIBOLD)
            .color(Studio::TEXT_PRIMARY)
            .width(Length::Fill)
            .ellipsis(1),
        Space::new().width(10),
        studio_button("Download", Message::Download(info))
            .icon(Icon::Download)
            .variant(StudioButtonVariant::Accent),
        Space::new().width(8),
        dismiss_button(),
    ]
    .align_y(Vertical::Center);
    // Coral at 10 %, with `Border(bottom: Studio.border)`: the line sits
    // under the padding.
    container(column![
        container(content)
            .width(Length::Fill)
            .padding(Padding::from([10, 24])),
        container(Space::new())
            .width(Length::Fill)
            .height(1)
            .style(theme::fill(Studio::BORDER)),
    ])
    .width(Length::Fill)
    .style(|_: &Theme| theme::boxed(Some(alpha(Studio::ACCENT, 0.10)), 0.0, None))
    .into()
}

/// A 28 px square with a 15 px cross, a click cursor and a "Dismiss" tooltip.
fn dismiss_button<'a>() -> Element<'a, Message> {
    let target = button(
        container(icons::icon(Icon::X, 15.0, Studio::TEXT_SECONDARY))
            .center_x(28)
            .center_y(28),
    )
    .padding(0)
    .style(bare_button)
    .on_press(Message::Dismiss);
    tooltip(
        target,
        container(text("Dismiss").size(theme::TOOLTIP_TEXT_SIZE))
            .padding(theme::TOOLTIP_PADDING)
            .style(theme::tooltip),
        tooltip::Position::Bottom,
    )
    // Material's tooltip sits 24 px below the target's centre.
    .gap(10)
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateDownloadDialog {
    info: UpdateInfo,
}

impl UpdateDownloadDialog {
    pub fn new(info: UpdateInfo) -> Self {
        Self { info }
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
            // The URL is read-only: an edit to it is dropped.
            DialogMessage::Input(_) => None,
        }
    }

    /// The dialog over the whole window: the barrier and the centred card
    /// (480 wide, the studio dialog card).
    pub fn view(&self) -> Element<'_, DialogMessage> {
        let info = &self.info;
        let mut content = column![text(format!("Vocara {} is available", info.version))
            .size(15)
            .font(font::BOLD)
            .color(Studio::TEXT_PRIMARY)];
        if !info.notes.is_empty() {
            content = content.push(Space::new().height(12)).push(
                container(
                    scrollable(
                        text(info.notes.as_str())
                            .size(12.5)
                            .line_height(1.5)
                            .color(Studio::TEXT_SECONDARY)
                            .width(Length::Fill),
                    )
                    .style(theme::scrollbar),
                )
                .max_height(200),
            );
        }
        // Selectable so the user can copy the address by hand if the browser
        // cannot be opened.
        let url = text_input("", &info.url)
            .on_input(DialogMessage::Input)
            .size(12)
            .padding(0)
            .style(url_style);
        content = content
            .push(Space::new().height(14))
            .push(
                container(url)
                    .width(Length::Fill)
                    .padding(Padding::from([10, 12]))
                    .style(theme::inset(8.0)),
            )
            .push(Space::new().height(22))
            .push(row![
                Space::new().width(Length::Fill),
                studio_button("Later", DialogMessage::Cancel),
                Space::new().width(10),
                studio_button("Open Download Page", DialogMessage::Confirm)
                    .icon(Icon::Download)
                    .variant(StudioButtonVariant::Accent),
            ]);
        let card = container(content.width(Length::Fill))
            .width(480)
            .padding(22)
            .style(theme::dialog_card);
        modal(container(card).padding(24).into())
    }
}

/// The URL: `Studio.textSecondary`, 12 px, on the inset well.
fn url_style(_: &Theme, _: text_input::Status) -> text_input::Style {
    text_input::Style {
        background: Background::Color(Color::TRANSPARENT),
        border: Border::default(),
        icon: Studio::TEXT_FAINT,
        placeholder: Studio::TEXT_FAINT,
        value: Studio::TEXT_SECONDARY,
        selection: alpha(Zinc::PRIMARY, 0.40),
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
            // The URL is in a (read-only) text field, not plain text.
            let url_field = ui.find(|candidate: Candidate<'_>| match candidate {
                Candidate::TextInput { state, .. }
                    if state.text() == "https://example.com/download" =>
                {
                    Some(())
                }
                _ => None,
            });
            assert!(url_field.is_ok(), "the URL is selectable");
            ui.click("Later").expect("the later button");
        });
        assert_eq!(messages, vec![DialogMessage::Cancel]);
        assert_eq!(
            dialog.update(DialogMessage::Cancel),
            Some(DialogOutcome::Close)
        );
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
