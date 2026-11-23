//! The view model's cases of `export_take_test.dart`: what is exported, the
//! names suggested, and what is refused before the save panel opens. (The
//! exporter's own rules — the quit held across the copy, one export at a
//! time, a source gone while the panel was open, a destination in the app's
//! storage or onto the source itself, cut names — are its own tests'.)

use std::sync::Arc;

use vocara_model::models::audio_track::{AudioTrack, AudioTrackKind};
use vocara_model::models::song_practice::SongPractice;

use super::*;

struct Files {
    dir: tempfile::TempDir,
}

impl Files {
    fn write(&self, name: &str, bytes: &[u8]) -> String {
        let path = self.dir.path().join(name);
        std::fs::write(&path, bytes).expect("a file");
        path.to_string_lossy().into_owned()
    }

    fn path(&self, name: &str) -> String {
        self.dir.path().join(name).to_string_lossy().into_owned()
    }
}

const TAKE: &[u8] = &[1, 2, 3, 4];
const CLIP_A: &[u8] = &[5, 6, 7];
const CLIP_B: &[u8] = &[8, 9];

/// "My Song": a single-clip take, a two-clip take, a backing (an mp3).
fn song(files: &Files) -> SongPractice {
    let mut p = SongPractice::new("p1", "My Song", at(), at());
    let single = take("single", "Take 1", &files.write("take.wav", TAKE));
    let multi = AudioTrack {
        clips: vec![
            clip("clip-a", &files.write("clip_a.wav", CLIP_A), 0, 300),
            clip("clip-b", &files.write("clip_b.wav", CLIP_B), 400, 300),
        ],
        ..AudioTrack::new("multi", AudioTrackKind::Vocal, "Take 2", at())
    };
    p.vocal_tracks = vec![Arc::new(single), Arc::new(multi)];
    p.accompaniment = Some(Arc::new(
        AudioTrack::new("acc", AudioTrackKind::Accompaniment, "Backing", at()).with_audio(
            files.write("backing.mp3", CLIP_A),
            0,
            500,
            Vec::new(),
        ),
    ));
    p
}

/// A screen whose save panel answers `pick` and records each suggestion.
fn open(
    practice: SongPractice,
    pick: Option<String>,
) -> (Fixture, Screen, Arc<Mutex<Vec<String>>>) {
    let picks = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&picks);
    let (f, s) = Fixture::open(Setup {
        practice,
        picker: Some(Arc::new(move |name: &str| {
            guard(&seen).push(name.to_string());
            Ok(pick.clone())
        })),
        ..Setup::default()
    });
    (f, s, picks)
}

#[test]
fn exports_a_single_clip_take_byte_for_byte_to_the_picked_path() {
    let files = Files {
        dir: tempfile::tempdir().expect("a temp dir"),
    };
    let dest = files.path("exported.wav");
    let (_f, mut s, picks) = open(song(&files), Some(dest.clone()));
    s.act(|vm| vm.export_track_audio("single"));
    s.settle();
    assert_eq!(*guard(&picks), ["My Song - Take 1.wav"]);
    assert_eq!(std::fs::read(&dest).expect("exported"), TAKE);
    assert!(s.toasts.contains(&format!("Exported to {dest}")));
}

#[test]
fn a_cancelled_save_panel_exports_nothing_and_stays_silent() {
    let files = Files {
        dir: tempfile::tempdir().expect("a temp dir"),
    };
    let (_f, mut s, picks) = open(song(&files), None);
    s.act(|vm| vm.export_track_audio("single"));
    s.settle();
    assert_eq!(guard(&picks).len(), 1, "the panel opened…");
    assert!(
        s.toasts.is_empty(),
        "…but a cancel is not an outcome to report"
    );
}

#[test]
fn refuses_a_multi_clip_take_without_opening_the_save_panel() {
    let files = Files {
        dir: tempfile::tempdir().expect("a temp dir"),
    };
    let dest = files.path("x.wav");
    let (_f, mut s, picks) = open(song(&files), Some(dest.clone()));
    s.act(|vm| vm.export_track_audio("multi"));
    s.settle();
    assert!(guard(&picks).is_empty());
    assert_eq!(s.toasts.len(), 1);
    assert!(s.toasts[0].contains("multiple clips"));
    assert!(!std::path::Path::new(&dest).exists());
}

#[test]
fn exports_one_clip_of_a_multi_clip_take_with_a_numbered_name() {
    let files = Files {
        dir: tempfile::tempdir().expect("a temp dir"),
    };
    let dest = files.path("clip_export.wav");
    let (_f, mut s, picks) = open(song(&files), Some(dest.clone()));
    s.act(|vm| vm.export_clip_audio("multi", "clip-b"));
    s.settle();
    assert_eq!(*guard(&picks), ["My Song - Take 2 (clip 2).wav"]);
    assert_eq!(std::fs::read(&dest).expect("exported"), CLIP_B);
}

#[test]
fn a_clip_is_numbered_by_where_it_sits_on_the_timeline() {
    // By storage order, a clip placed before the take's first was "clip 2".
    let files = Files {
        dir: tempfile::tempdir().expect("a temp dir"),
    };
    let mut p = song(&files);
    let multi = Arc::clone(&p.vocal_tracks[1]);
    let mut reversed = (*multi).clone();
    reversed.clips.reverse();
    p.vocal_tracks[1] = Arc::new(reversed);
    let (_f, mut s, picks) = open(p, None);
    s.act(|vm| vm.export_clip_audio("multi", "clip-a"));
    s.settle();
    assert_eq!(*guard(&picks), ["My Song - Take 2 (clip 1).wav"]);
}

#[test]
fn keeps_the_source_extension_for_a_backing_export() {
    let files = Files {
        dir: tempfile::tempdir().expect("a temp dir"),
    };
    let dest = files.path("backing_export.mp3");
    let (_f, mut s, picks) = open(song(&files), Some(dest.clone()));
    s.act(|vm| vm.export_track_audio("acc"));
    s.settle();
    assert_eq!(*guard(&picks), ["My Song - Backing.mp3"]);
    assert!(std::path::Path::new(&dest).exists());
}

#[test]
fn a_missing_source_file_toasts_instead_of_opening_the_panel() {
    let files = Files {
        dir: tempfile::tempdir().expect("a temp dir"),
    };
    let (_f, mut s, picks) = open(song(&files), Some(files.path("y.wav")));
    std::fs::remove_file(files.path("take.wav")).expect("removed");
    s.act(|vm| vm.export_track_audio("single"));
    s.settle();
    assert!(guard(&picks).is_empty());
    assert_eq!(s.toasts.len(), 1);
    assert!(s.toasts[0].contains("missing"));
}

#[test]
fn an_empty_take_has_no_audio_to_export_yet() {
    let files = Files {
        dir: tempfile::tempdir().expect("a temp dir"),
    };
    let mut p = song(&files);
    p.vocal_tracks.push(Arc::new(AudioTrack::new(
        "empty",
        AudioTrackKind::Vocal,
        "Take 3",
        at(),
    )));
    let (_f, mut s, picks) = open(p, None);
    s.act(|vm| vm.export_track_audio("empty"));
    assert!(guard(&picks).is_empty());
    assert_eq!(s.toasts, ["This take has no audio to export yet."]);
}

#[test]
fn separation_stays_gated_probe_false_and_a_plain_language_toast() {
    let files = Files {
        dir: tempfile::tempdir().expect("a temp dir"),
    };
    let (_f, mut s, _picks) = open(song(&files), None);
    // The unavailable engine resolves the probe to false, which is what
    // hides "Extract backing" in the menu.
    assert!(!s.vm.separation_available());
    // Residual race (a click before the probe landed): user language.
    s.act(|vm| vm.separate_backing("acc"));
    assert_eq!(
        s.toasts,
        ["Source separation is not available in this version."]
    );
    assert!(!s.vm.is_separating());
}
