use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use super::fake::FakeFilePanels;
use super::*;

/// The value of a future that is ready at once (a fake's answer is).
fn ready<T>(future: impl Future<Output = T>) -> T {
    let mut future = pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("a fake panel's answer is ready at once"),
    }
}

fn filter(name: &str, extensions: &[&str]) -> Filter {
    Filter {
        name: name.to_string(),
        extensions: extensions.iter().map(|e| e.to_string()).collect(),
    }
}

#[test]
fn the_builder_records_what_rfd_was_given() {
    let extensions = vec!["mp4".to_string(), "mov".to_string()];
    let dialog = AsyncFileDialog::new()
        .add_filter("Audio", &["wav", "mp3"])
        .add_filter("Video", &extensions)
        .set_directory("/tmp/clips")
        .set_file_name("take.wav")
        .set_title("Pick a clip")
        .set_can_create_directories(false);
    assert_eq!(
        dialog.panel(),
        &Panel {
            title: Some("Pick a clip".to_string()),
            directory: Some(PathBuf::from("/tmp/clips")),
            file_name: Some("take.wav".to_string()),
            filters: vec![
                filter("Audio", &["wav", "mp3"]),
                filter("Video", &["mp4", "mov"]),
            ],
            can_create_directories: Some(false),
        }
    );
    // The filters together are what a macOS panel allows.
    assert_eq!(
        dialog.panel().allowed_file_types(),
        ["wav", "mp3", "mp4", "mov"]
    );
}

#[test]
fn an_empty_directory_is_none_and_nothing_set_is_the_default() {
    let dialog = FileDialog::new().set_directory("");
    assert_eq!(dialog.panel(), &Panel::default());
    assert_eq!(dialog.panel().directory, None);
    assert_eq!(dialog.panel().can_create_directories, None);
    // Filters with no extensions allow nothing in particular (AppKit
    // raises for an empty list of types, so none is set).
    let empty = FileDialog::new().add_filter("None", &[] as &[&str]);
    assert!(empty.panel().allowed_file_types().is_empty());
}

#[test]
fn a_fake_answers_a_save_panel_and_records_it() {
    let fake = FakeFilePanels::install();
    fake.pick(["/out/take.wav", "/out/ignored.wav"]);
    let saved = FileDialog::new().set_file_name("take.wav").save_file();
    assert_eq!(saved, Some(PathBuf::from("/out/take.wav")));
    // Nothing queued: the next one is cancelled.
    assert_eq!(FileDialog::new().save_file(), None);
    assert_eq!(
        fake.requests(),
        [
            Request {
                kind: Kind::SaveFile,
                panel: Panel {
                    file_name: Some("take.wav".to_string()),
                    ..Panel::default()
                },
            },
            Request {
                kind: Kind::SaveFile,
                panel: Panel::default(),
            },
        ]
    );
}

#[test]
fn a_fake_answers_the_open_panels_at_the_call() {
    let fake = FakeFilePanels::install();
    fake.pick(["/in/a.lrc"]);
    fake.cancel();
    fake.pick(["/in/a.wav", "/in/b.wav"]);
    let one = AsyncFileDialog::new()
        .add_filter("Lyrics", &["lrc", "txt"])
        .pick_file();
    let cancelled = AsyncFileDialog::new().pick_files();
    let many = AsyncFileDialog::new()
        .add_filter("Audio", &["wav"])
        .pick_files();
    // Recorded when the panel is asked for, before anything awaits it.
    let kinds: Vec<Kind> = fake.requests().iter().map(|r| r.kind).collect();
    assert_eq!(kinds, [Kind::PickFile, Kind::PickFiles, Kind::PickFiles]);
    assert_eq!(
        fake.requests()[0].panel.filters,
        [filter("Lyrics", &["lrc", "txt"])]
    );
    assert_eq!(
        ready(one).as_ref().map(FileHandle::path),
        Some(Path::new("/in/a.lrc"))
    );
    assert_eq!(ready(cancelled), None);
    let many = ready(many).unwrap_or_default();
    let paths: Vec<&Path> = many.iter().map(FileHandle::path).collect();
    assert_eq!(paths, [Path::new("/in/a.wav"), Path::new("/in/b.wav")]);
}

#[test]
fn a_fake_answers_panels_asked_for_on_other_threads() {
    let fake = FakeFilePanels::install();
    fake.pick(["/out/diagnostics.zip"]);
    let saved = std::thread::spawn(|| {
        FileDialog::new()
            .set_file_name("diagnostics.zip")
            .save_file()
    })
    .join()
    .expect("the worker ran");
    assert_eq!(saved, Some(PathBuf::from("/out/diagnostics.zip")));
    assert_eq!(fake.requests().len(), 1);
}
