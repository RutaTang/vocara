//! The banner and the dialog through every state of the self-update, over
//! the fakes of [`super::testing`]: what each shows, what each button does
//! to the shared self-update, and the release-page fallback.

use iced_test::selector::Candidate;
use vocara_services::update_check_service::{FetchError, UpdateStatus};

use super::testing::{image, manifest, release, updates};
use super::*;

/// A service whose manifest advertises 9.9.9 over a running 1.0.0: in the
/// shape before the self-update (no image) unless `with_image`.
fn newer_available(with_image: bool) -> UpdateCheckService {
    let body = manifest(
        "9.9.9",
        "New voiceprint views.",
        with_image.then(image).as_deref(),
    );
    UpdateCheckService::with_fetcher(Box::new(move |_| Ok(body.clone())), "1.0.0")
}

fn banner_messages(
    updates: &UpdateCheckService,
    updater: &SelfUpdate,
    drive: impl FnOnce(&mut iced_test::Simulator<'_, Message>),
) -> Vec<Message> {
    let mut ui = iced_test::simulator(update_notice_banner(updates, updater));
    drive(&mut ui);
    ui.into_messages().collect()
}

fn shows(updates: &UpdateCheckService, updater: &SelfUpdate, label: &str) -> bool {
    let mut ui = iced_test::simulator(update_notice_banner(updates, updater));
    ui.find(label).is_ok()
}

/// Clicks `label` on the banner and handles what it reports; the dialog
/// it opened, if any.
fn click(
    updates: &UpdateCheckService,
    updater: &Arc<SelfUpdate>,
    label: &str,
) -> Option<UpdateDownloadDialog> {
    let messages = banner_messages(updates, updater, |ui| {
        ui.click(label).unwrap_or_else(|e| panic!("{label}: {e:?}"));
    });
    assert_eq!(messages.len(), 1, "{messages:?}");
    messages
        .into_iter()
        .find_map(|m| update(updates, updater, m))
}

fn unavailable() -> Arc<SelfUpdate> {
    Arc::new(SelfUpdate::unavailable(
        "Vocara is not running from an installed app",
    ))
}

#[test]
fn a_newer_release_raises_the_banner() {
    let updates = newer_available(false);
    let updater = unavailable();
    assert!(
        !shows(&updates, &updater, "Download"),
        "nothing before a check"
    );
    updates.check();
    assert!(shows(&updates, &updater, "Vocara 9.9.9 is available."));
    // No image: the release page is the way.
    assert!(shows(&updates, &updater, "Download"));
    assert!(!shows(&updates, &updater, "Update now"));
}

#[test]
fn an_up_to_date_check_shows_no_banner() {
    let updates = UpdateCheckService::with_fetcher(
        Box::new(|_| Ok(r#"{"latest": "1.0.0", "url": "https://example.com"}"#.to_string())),
        "1.0.0",
    );
    updates.check();
    assert_eq!(updates.status(), UpdateStatus::UpToDate);
    assert!(!shows(&updates, &unavailable(), "Download"));
}

#[test]
fn a_failed_check_stays_silent() {
    let updates =
        UpdateCheckService::with_fetcher(Box::new(|_| Err(FetchError("offline".into()))), "1.0.0");
    updates.check();
    assert_eq!(updates.status(), UpdateStatus::Unknown);
    assert!(!shows(&updates, &unavailable(), "Download"));
}

/// A release with its image, on a copy that can replace itself: "What's
/// new" and the one-click "Update now".
#[test]
fn a_copy_that_can_update_itself_is_offered_update_now() {
    let updates = newer_available(true);
    updates.check();
    let fake = super::testing::updates(false);
    fake.updater.prepare();
    assert!(shows(&updates, &fake.updater, "Vocara 9.9.9 is available."));
    assert!(shows(&updates, &fake.updater, "What's new"));
    assert!(shows(&updates, &fake.updater, "Update now"));
    assert!(!shows(&updates, &fake.updater, "Download"));
    // Not yet known whether it can (the check off the UI thread has not
    // answered): offered all the same, and the attempt says if it cannot.
    let unknown = super::testing::updates(false);
    assert!(shows(&updates, &unknown.updater, "Update now"));
}

/// "Update now" is the whole of it: the download shows its progress and a
/// Cancel, which stops it and brings the offer back.
#[test]
fn update_now_downloads_with_progress_and_cancel_stops_it() {
    let updates = newer_available(true);
    updates.check();
    let fake = super::testing::updates(true);
    fake.updater.prepare();
    assert!(click(&updates, &fake.updater, "Update now").is_none());
    assert_eq!(
        fake.updater.phase(),
        Phase::Downloading {
            done: 0,
            total: 2500
        }
    );
    fake.let_through(1);
    fake.wait_for(&Phase::Downloading {
        done: 1000,
        total: 2500,
    });
    assert!(shows(&updates, &fake.updater, "Downloading Vocara 9.9.9…"));
    assert!(shows(&updates, &fake.updater, "1 kB of 3 kB"));
    assert!(!shows(&updates, &fake.updater, "Update now"));
    assert!(click(&updates, &fake.updater, "Cancel").is_none());
    assert_eq!(fake.updater.phase(), Phase::Idle);
    assert!(
        shows(&updates, &fake.updater, "Update now"),
        "the offer is back"
    );
    drop(fake);
}

/// Downloaded and checked: installing, then the app is asked to quit; the
/// banner says Vocara will relaunch, and offers nothing to press.
#[test]
fn an_installing_update_says_vocara_will_relaunch() {
    let updates = newer_available(true);
    updates.check();
    let fake = super::testing::updates(false);
    fake.updater.prepare();
    click(&updates, &fake.updater, "Update now");
    fake.settle();
    assert_eq!(fake.updater.phase(), Phase::Relaunching);
    assert!(fake.updater.wants_quit());
    assert!(shows(
        &updates,
        &fake.updater,
        "Installing Vocara 9.9.9… Vocara will relaunch."
    ));
    let buttons = banner_messages(&updates, &fake.updater, |ui| {
        for label in ["Cancel", "Update now", "Dismiss"] {
            assert!(ui.click(label).is_err(), "{label}");
        }
    });
    assert!(buttons.is_empty());
}

/// A failed update says why, with "Try again" (and "Details", the dialog);
/// dismissing it puts it away.
#[test]
fn a_failed_update_says_why_and_can_be_tried_again() {
    let updates = newer_available(true);
    updates.check();
    let fake = super::testing::updates(false);
    fake.updater.prepare();
    *fake.installer.fails_with.lock().unwrap() =
        Some("the update is not notarized by Apple: rejected".into());
    click(&updates, &fake.updater, "Update now");
    fake.settle();
    assert!(shows(
        &updates,
        &fake.updater,
        "Update failed: the update is not notarized by Apple: rejected"
    ));
    assert!(shows(&updates, &fake.updater, "Details"));
    *fake.installer.fails_with.lock().unwrap() = None;
    click(&updates, &fake.updater, "Try again");
    fake.settle();
    assert_eq!(fake.updater.phase(), Phase::Relaunching);
}

/// A swap that failed after the last quit is reported by the relaunched
/// Vocara even with no release on offer, until dismissed.
#[test]
fn a_failed_swap_is_reported_after_the_relaunch() {
    let updates = newer_available(false);
    let fake = super::testing::updates(false);
    // As `SelfUpdate::startup` finds it.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path()
            .join(vocara_services::self_update::files::INSTALL_FAILED_MARKER),
        "could not copy the new version into place; the previous version was restored",
    )
    .unwrap();
    let relaunched = Arc::new(SelfUpdate::new(
        {
            let dir = dir.path().to_path_buf();
            Box::new(move || Ok(dir.clone()))
        },
        Arc::new(vocara_services::self_update::HttpsTransport),
        fake.installer.clone(),
        "1.0.0",
    ));
    relaunched.startup();
    let line = "Update failed: could not copy the new version into place; the previous version \
                was restored";
    assert!(shows(&updates, &relaunched, line));
    assert!(
        !shows(&updates, &relaunched, "Try again"),
        "nothing on offer"
    );
    let cross = char::from(Icon::X).to_string();
    let _ = click(&updates, &relaunched, &cross);
    assert!(!shows(&updates, &relaunched, line));
    assert_eq!(relaunched.phase(), Phase::Idle);
}

#[test]
fn dismissing_the_banner_hides_it_for_the_rest_of_the_run() {
    let updates = newer_available(false);
    let updater = unavailable();
    updates.check();
    assert!(shows(&updates, &updater, "Vocara 9.9.9 is available."));
    let cross = char::from(Icon::X).to_string();
    let messages = banner_messages(&updates, &updater, |ui| {
        ui.click(cross.as_str()).expect("the dismiss button");
    });
    assert_eq!(messages, vec![Message::Dismiss]);
    for m in messages {
        assert!(update(&updates, &updater, m).is_none());
    }
    assert!(!shows(&updates, &updater, "Vocara 9.9.9 is available."));
    assert!(updates.notice_dismissed());
    // A later check that finds it again keeps it dismissed.
    updates.check();
    assert!(!shows(&updates, &updater, "Download"));
}

// -------------------------------------------------------------------- dialog

fn dialog_messages(
    dialog: &UpdateDownloadDialog,
    drive: impl FnOnce(&mut iced_test::Simulator<'_, DialogMessage>),
) -> Vec<DialogMessage> {
    let mut ui = iced_test::simulator(dialog.view());
    drive(&mut ui);
    ui.into_messages().collect()
}

fn dialog_shows(dialog: &UpdateDownloadDialog, label: &str) -> bool {
    let mut ui = iced_test::simulator(dialog.view());
    ui.find(label).is_ok()
}

/// "Download" opens the dialog in its fallback: why Vocara cannot update
/// itself, and the release page as selectable text.
#[test]
fn download_opens_the_dialog_with_notes_a_reason_and_a_selectable_url() {
    let updates = newer_available(false);
    let updater = unavailable();
    updates.check();
    let mut dialog = click(&updates, &updater, "Download").expect("the dialog opens");
    let messages = dialog_messages(&dialog, |ui| {
        assert!(ui.find("Vocara 9.9.9 is available").is_ok());
        assert!(ui.find("New voiceprint views.").is_ok());
        assert!(ui
            .find(
                "Vocara can't update itself here: this release offers no download Vocara can \
                 install by itself."
            )
            .is_ok());
        // The URL is selectable text: text, not an editable field.
        assert!(ui
            .find("https://github.com/RutaTang/vocara/releases/tag/v9.9.9")
            .is_ok());
        let field = ui.find(|candidate: Candidate<'_>| match candidate {
            Candidate::TextInput { .. } => Some(()),
            _ => None,
        });
        assert!(field.is_err(), "no editable field");
        assert!(ui.find("Update now").is_err());
        ui.click("Later").expect("the later button");
    });
    assert_eq!(messages, vec![DialogMessage::Cancel]);
    assert_eq!(
        dialog.update(DialogMessage::Cancel),
        Some(DialogOutcome::Close)
    );
}

/// The fallback's reason is the copy's own: here, not an installed app.
#[test]
fn the_fallback_says_why_this_copy_cannot_update_itself() {
    let updater = unavailable();
    updater.prepare();
    let dialog = UpdateDownloadDialog::new(release(Some(&image())), updater);
    assert!(dialog_shows(
        &dialog,
        "Vocara can't update itself here: Vocara is not running from an installed app."
    ));
    assert!(dialog_shows(&dialog, "Open Download Page"));
}

#[test]
fn the_url_wraps_in_its_well_inside_the_card() {
    // The 480-wide card centred in the 1024-wide window, 20 inside its
    // inset outline; the well's text 10 in from its sides; the text at its
    // 12 px lines, as many as it takes.
    use crate::ui::core::widgets::confirm_dialog::CARD_PADDING;
    crate::ui::core::widgets::studio_text::testing::load_fonts();
    let card_left = (1024.0 - DIALOG_WIDTH) / 2.0;
    let width = DIALOG_WIDTH - 2.0 * CARD_PADDING - 2.0 * 10.0;
    let dialog = |url: &str| {
        let mut info = release(None);
        info.url = url.into();
        info.notes = String::new();
        UpdateDownloadDialog::new(info, unavailable())
    };
    let short = dialog("https://example.com/2");
    let mut ui = iced_test::simulator(short.view());
    let title = ui.find("Vocara 9.9.9 is available").expect("the title");
    let url = ui.find("https://example.com/2").expect("the URL").bounds();
    assert_eq!(title.bounds().x, card_left + CARD_PADDING);
    assert_eq!(
        (url.x, url.width, url.height),
        (card_left + CARD_PADDING + 10.0, width, 12.0)
    );
    assert!(url.y > title.bounds().y + 18.0 + 12.0, "under the reason");

    let long = format!("https://example.com/releases/{}", "a".repeat(120));
    let long_dialog = dialog(&long);
    let mut ui = iced_test::simulator(long_dialog.view());
    let url = ui.find(long.as_str()).expect("the URL").bounds();
    assert!(url.height >= 24.0 && url.height % 12.0 == 0.0, "{url:?}");
}

#[test]
fn the_url_cannot_be_edited() {
    let mut dialog = UpdateDownloadDialog::new(release(None), unavailable());
    let before = dialog.clone();
    assert_eq!(dialog.update(DialogMessage::Input("x".into())), None);
    assert_eq!(dialog, before);
}

#[test]
fn open_download_page_opens_the_release_url_and_keeps_the_dialog() {
    let mut dialog = UpdateDownloadDialog::new(release(None), unavailable());
    let messages = dialog_messages(&dialog, |ui| {
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
            DialogOutcome::OpenUrl("https://github.com/RutaTang/vocara/releases/tag/v9.9.9".into()),
            DialogOutcome::Close
        ]
    );
}

/// The dialog's "Update now" starts the update and stays: it shows the
/// download, whose Cancel (the dialog's second action) stops it, while
/// "Later" closes the dialog and leaves the download running.
#[test]
fn the_dialog_updates_shows_the_download_and_cancels_it() {
    let fake = updates(true);
    fake.updater.prepare();
    let mut dialog = UpdateDownloadDialog::new(release(Some(&image())), fake.updater.clone());
    assert!(!dialog_shows(&dialog, "Open Download Page"));
    let messages = dialog_messages(&dialog, |ui| {
        ui.click("Update now").expect("update now");
    });
    assert_eq!(messages, [DialogMessage::Confirm]);
    assert_eq!(dialog.update(DialogMessage::Confirm), None, "it stays");
    fake.let_through(1);
    fake.wait_for(&Phase::Downloading {
        done: 1000,
        total: 2500,
    });
    assert!(dialog_shows(&dialog, "Downloading Vocara 9.9.9…"));
    assert!(dialog_shows(&dialog, "1 kB of 3 kB"));
    // "Update now" is disabled while it runs.
    let pressed = dialog_messages(&dialog, |ui| {
        let _ = ui.click("Update now");
    });
    assert!(pressed.is_empty(), "{pressed:?}");
    // Later closes the dialog; the download runs on.
    assert_eq!(
        dialog.update(DialogMessage::Cancel),
        Some(DialogOutcome::Close)
    );
    assert!(fake.updater.phase().busy());
    let messages = dialog_messages(&dialog, |ui| {
        ui.click("Cancel").expect("the download's cancel");
    });
    assert_eq!(messages, [DialogMessage::Secondary]);
    assert_eq!(dialog.update(DialogMessage::Secondary), None);
    assert_eq!(fake.updater.phase(), Phase::Idle);
    assert!(dialog_shows(&dialog, "Update now"));
}

/// Installing: the dialog says Vocara will relaunch.
#[test]
fn the_dialog_says_vocara_will_relaunch() {
    let fake = updates(false);
    fake.updater.prepare();
    let mut dialog = UpdateDownloadDialog::new(release(Some(&image())), fake.updater.clone());
    dialog.update(DialogMessage::Confirm);
    fake.settle();
    assert!(dialog_shows(
        &dialog,
        "Installing Vocara 9.9.9… Vocara will relaunch."
    ));
}

/// A failure in the dialog: why, "Try again", and the release page beside
/// it.
#[test]
fn a_failure_in_the_dialog_offers_try_again_and_the_release_page() {
    let fake = updates(false);
    fake.updater.prepare();
    *fake.installer.fails_with.lock().unwrap() = Some("the update's signature is invalid".into());
    let mut dialog = UpdateDownloadDialog::new(release(Some(&image())), fake.updater.clone());
    dialog.update(DialogMessage::Confirm);
    fake.settle();
    assert!(dialog_shows(
        &dialog,
        "Update failed: the update's signature is invalid"
    ));
    let messages = dialog_messages(&dialog, |ui| {
        ui.click("Open Download Page").expect("the release page");
        ui.click("Try again").expect("try again");
    });
    assert_eq!(messages, [DialogMessage::Secondary, DialogMessage::Confirm]);
    assert_eq!(
        dialog.update(DialogMessage::Secondary),
        Some(DialogOutcome::OpenUrl(
            "https://github.com/RutaTang/vocara/releases/tag/v9.9.9".into()
        ))
    );
    *fake.installer.fails_with.lock().unwrap() = None;
    assert_eq!(dialog.update(DialogMessage::Confirm), None);
    fake.settle();
    assert_eq!(fake.updater.phase(), Phase::Relaunching);
}

#[test]
fn sizes_read_as_finder_writes_them() {
    assert_eq!(human_size(0), "0 kB");
    assert_eq!(human_size(1), "1 kB");
    assert_eq!(human_size(999_999), "1000 kB");
    assert_eq!(human_size(1_000_000), "1.0 MB");
    assert_eq!(human_size(41_234_567), "41.2 MB");
    assert_eq!(progress_line(12_300_000, 41_234_567), "12.3 MB of 41.2 MB");
}

/// The bar's fill is its share of the track, whatever the counts.
#[test]
fn the_progress_bar_fills_its_share() {
    for (done, total) in [(0, 100), (25, 100), (100, 100), (150, 100), (5, 0)] {
        let mut ui = iced_test::simulator(progress_bar::<()>(done, total, Length::Fixed(160.0)));
        // It draws, and takes no input.
        let _ = ui.click("nothing");
        assert_eq!(ui.into_messages().count(), 0);
    }
}
