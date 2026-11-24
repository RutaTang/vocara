//! The open and save panels the app asks for, through
//! `vocara_services::file_panels`' test fake: what each call site sets up
//! (the configuration rfd was given before the panels were the app's own)
//! and that what the panel picks comes back as the call site's message.

use std::path::PathBuf;
use std::sync::Arc;

use iced::futures::StreamExt;
use iced::Task;
use iced_test::runtime::Action;
use vocara_app::app::library::Library;
use vocara_app::app::AppEnvironment;
use vocara_app::services::Services;
use vocara_app::ui::settings::settings_screen::SettingsHooks;
use vocara_app::ui::song_practice_detail::widgets::lyrics_panel::{self, LYRICS_FILE_EXTENSIONS};
use vocara_app::ui::technique_library::widgets::media::technique_media_section as media_section;
use vocara_model::models::technique_media::TechniqueMedia;
use vocara_services::file_panels::fake::FakeFilePanels;
use vocara_services::file_panels::{Filter, Kind, Panel, Request};

/// The messages `task` produces, run to completion.
fn run<M: Send + 'static>(task: Task<M>) -> Vec<M> {
    let Some(stream) = iced_test::runtime::task::into_stream(task) else {
        return Vec::new();
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a test runtime");
    runtime.block_on(
        stream
            .filter_map(|action| async move {
                match action {
                    Action::Output(m) => Some(m),
                    _ => None,
                }
            })
            .collect(),
    )
}

fn open_panel(kind: Kind, name: &str, extensions: Vec<String>) -> Request {
    Request {
        kind,
        panel: Panel {
            filters: vec![Filter {
                name: name.to_string(),
                extensions,
            }],
            ..Panel::default()
        },
    }
}

#[test]
fn the_lyrics_import_asks_for_one_lyrics_file() {
    let panels = FakeFilePanels::install();
    panels.pick(["/songs/verse.lrc"]);
    let task = lyrics_panel::browse_lyrics_file();
    // Asked for at the call, in `update`, as rfd's panel was.
    assert_eq!(
        panels.requests(),
        [open_panel(
            Kind::PickFile,
            "Lyrics",
            LYRICS_FILE_EXTENSIONS.map(String::from).to_vec()
        )]
    );
    assert_eq!(
        run(task),
        [lyrics_panel::Message::ImportLyricsFromFile(PathBuf::from(
            "/songs/verse.lrc"
        ))]
    );
}

#[test]
fn a_cancelled_lyrics_import_reports_nothing() {
    let panels = FakeFilePanels::install();
    panels.cancel();
    assert!(run(lyrics_panel::browse_lyrics_file()).is_empty());
    assert_eq!(panels.requests().len(), 1);
}

#[test]
fn the_examples_browse_asks_for_any_number_of_clips() {
    let panels = FakeFilePanels::install();
    panels.pick(["/clips/a.wav", "/clips/b.mov"]);
    let task = media_section::browse();
    assert_eq!(
        panels.requests(),
        [open_panel(
            Kind::PickFiles,
            "Audio or video",
            TechniqueMedia::picker_extensions()
        )]
    );
    assert_eq!(
        run(task),
        [media_section::Message::Picked(vec![
            "/clips/a.wav".to_string(),
            "/clips/b.mov".to_string()
        ])]
    );
}

#[test]
fn the_settings_save_panel_suggests_the_name_and_nothing_else() {
    let panels = FakeFilePanels::install();
    panels.pick(["/exports/vocara-diagnostics.zip"]);
    panels.cancel();
    let services = Services::detached(Arc::new(Library::in_memory()), AppEnvironment::Dev);
    let pick = SettingsHooks::production(&services).pick_save_path;
    assert_eq!(
        pick("vocara-diagnostics.zip"),
        Ok(Some("/exports/vocara-diagnostics.zip".to_string()))
    );
    assert_eq!(pick("vocara-diagnostics.zip"), Ok(None));
    let asked = Request {
        kind: Kind::SaveFile,
        panel: Panel {
            file_name: Some("vocara-diagnostics.zip".to_string()),
            ..Panel::default()
        },
    };
    assert_eq!(panels.requests(), [asked.clone(), asked]);
}
