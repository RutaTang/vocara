//! Port of `selection_repair_test.dart`: the selection the inspector and the
//! dock follow, kept on a lane that exists and is on screen whenever the
//! model changes under it — a deleted take, a reverted or remotely deleted
//! bus, the backing lane switching sides, a save that merged in someone
//! else's take.

use std::sync::Arc;

use vocara_model::models::audio_track::{AudioTrack, AudioTrackKind};
use vocara_model::models::song_practice::SongPractice;

use super::*;

fn practice(
    takes: Option<Vec<AudioTrack>>,
    versions: Option<Vec<Arc<TakeVersion>>>,
) -> SongPractice {
    let mut p = SongPractice::new("p1", "Song", at(), at());
    p.active_version_id = versions
        .as_ref()
        .and_then(|v| v.first().map(|v| v.id.clone()));
    p.versions = versions.unwrap_or_default();
    p.vocal_tracks = takes
        .unwrap_or_else(|| {
            vec![
                take("t2", "t2", "/pool/t2.wav"),
                take("t1", "t1", "/pool/t1.wav"),
            ]
        })
        .into_iter()
        .map(Arc::new)
        .collect();
    p
}

fn backing(id: &str, kind: AudioTrackKind) -> Arc<AudioTrack> {
    Arc::new(AudioTrack::new(id, kind, id, at()).with_audio(
        format!("/pool/{id}.wav"),
        0,
        1000,
        Vec::new(),
    ))
}

fn open(p: SongPractice, spectro: &Arc<CountingSpectro>) -> (Fixture, Screen) {
    Fixture::open(Setup {
        practice: p,
        spectrograms: Some(Arc::clone(spectro) as Arc<dyn SpectrogramAnalyses>),
        ..Setup::default()
    })
}

/// The inspector collapsed and the dock open on an analysis tab: the dock
/// computes a take's analysis only when asked.
fn dock_on_pitch(s: &mut Screen, spectro: &CountingSpectro) {
    s.act(|vm| vm.toggle_inspector_panel());
    s.act(|vm| vm.analysis_do(|a, h| a.show_dock_tab(DockTab::Pitch, h)));
    s.settle();
    guard(&spectro.asked).clear();
}

#[test]
fn deleting_the_selected_take_analyzes_the_take_the_selection_moves_to() {
    let spectro = Arc::new(CountingSpectro::default());
    let (_f, mut s) = open(practice(None, None), &spectro);
    assert_eq!(s.vm.selected_track_id(), Some("t2"));
    dock_on_pitch(&mut s, &spectro);
    s.act(|vm| vm.delete_track("t2"));
    s.settle();
    assert_eq!(s.vm.selected_track_id(), Some("t1"));
    assert!(guard(&spectro.asked).contains(&"/pool/t1.wav".to_string()));
}

#[test]
fn deleting_the_active_interpretation_analyzes_the_take_it_falls_back_to() {
    let spectro = Arc::new(CountingSpectro::default());
    let mut ta = take("ta", "ta", "/pool/ta.wav");
    ta.version_id = Some("A".into());
    let mut tb = take("tb", "tb", "/pool/tb.wav");
    tb.version_id = Some("B".into());
    let (_f, mut s) = open(
        practice(
            Some(vec![ta, tb]),
            Some(vec![version("A", "A"), version("B", "B")]),
        ),
        &spectro,
    );
    assert_eq!(s.vm.selected_track_id(), Some("ta"));
    dock_on_pitch(&mut s, &spectro);
    s.act(|vm| vm.versions_do(|v, h| v.delete("A", h)));
    s.settle();
    assert_eq!(s.vm.selected_track_id(), Some("tb"));
    assert!(guard(&spectro.asked).contains(&"/pool/tb.wav".to_string()));
}

#[test]
fn a_selected_bus_that_no_longer_exists_gives_the_selection_back_to_a_lane() {
    let spectro = Arc::new(CountingSpectro::default());
    let (f, mut s) = open(practice(None, None), &spectro);
    s.act(|vm| vm.select_track("t1"));
    f.repo.s().fail_next_save = true;
    s.act(|vm| vm.mixer_do(|m, h| m.create_bus(h, "X")));
    s.settle();
    assert!(s.practice().fx_buses.is_empty(), "reverted");
    s.act(|vm| vm.rename_track("t1", "renamed")); // any later save
    s.settle();
    assert!(s.vm.selected_bus_id().is_none() || s.vm.selected_bus().is_some());
    assert!(s.vm.selected_track_id().is_some());
}

#[test]
fn a_practice_holding_only_the_original_never_selects_its_hidden_lane() {
    let spectro = Arc::new(CountingSpectro::default());
    let mut p = practice(None, None);
    p.reference = Some(backing("ref", AudioTrackKind::Reference));
    let (_f, s) = open(p, &spectro);
    assert_ne!(s.vm.selected_track_id(), Some("ref"));
}

#[test]
fn a_save_that_folds_in_a_long_take_grows_the_timeline_to_hold_it() {
    let spectro = Arc::new(CountingSpectro::default());
    let (f, mut s) = open(practice(None, None), &spectro);
    f.repo.s().fold_in_on_next_save = Some(
        AudioTrack::new("remote", AudioTrackKind::Vocal, "Remote take", at()).with_audio(
            "/pool/remote.wav",
            0,
            120_000,
            Vec::new(),
        ),
    );
    s.act(|vm| vm.rename_track("t1", "renamed"));
    s.settle();
    let p = s.practice();
    assert!(p.timeline_duration_ms >= p.content_end_ms());
}

#[test]
fn a_selection_on_the_backing_lane_follows_the_backing_it_shows() {
    let spectro = Arc::new(CountingSpectro::default());
    let mut p = practice(None, None);
    p.accompaniment = Some(backing("acc", AudioTrackKind::Accompaniment));
    p.reference = Some(backing("ref", AudioTrackKind::Reference));
    let (_f, mut s) = open(p, &spectro);
    s.act(|vm| vm.select_track("acc"));
    s.act(|vm| vm.set_play_reference(true));
    assert_eq!(s.vm.selected_track_id(), Some("ref"));
    s.act(|vm| vm.set_play_reference(false));
    assert_eq!(s.vm.selected_track_id(), Some("acc"));
    // A take's selection stays put.
    s.act(|vm| vm.select_track("t1"));
    s.act(|vm| vm.set_play_reference(true));
    assert_eq!(s.vm.selected_track_id(), Some("t1"));
}

#[test]
fn a_backing_switch_that_moves_the_selection_analyzes_where_it_lands_and_comes_back() {
    // A practice with an accompaniment and no original: switching to the
    // empty Original slot moves the selection to a take. The dock (open on an
    // analysis tab) computes only when asked, and nothing asked.
    let spectro = Arc::new(CountingSpectro::default());
    let mut p = practice(None, None);
    p.accompaniment = Some(backing("acc", AudioTrackKind::Accompaniment));
    let (_f, mut s) = open(p, &spectro);
    s.act(|vm| vm.select_track("acc"));
    dock_on_pitch(&mut s, &spectro);
    s.act(|vm| vm.set_play_reference(true));
    s.settle();
    assert_eq!(s.vm.selected_track_id(), Some("t2"));
    assert!(guard(&spectro.asked).contains(&"/pool/t2.wav".to_string()));
    s.act(|vm| vm.set_play_reference(false));
    assert_eq!(
        s.vm.selected_track_id(),
        Some("acc"),
        "a glance at the empty slot"
    );
}

#[test]
fn a_pick_of_the_users_own_ends_a_backing_detour_and_re_tapping_changes_nothing() {
    let spectro = Arc::new(CountingSpectro::default());
    let mut p = practice(None, None);
    p.accompaniment = Some(Arc::new(AudioTrack {
        clips: vec![clip("acc-c", "/pool/acc.wav", 0, 1000)],
        ..AudioTrack::new("acc", AudioTrackKind::Accompaniment, "acc", at())
    }));
    let (_f, mut s) = open(p, &spectro);
    s.act(|vm| vm.select_track("acc"));
    s.act(|vm| vm.set_play_reference(true)); // the empty Original slot: off to a take
    assert_eq!(s.vm.selected_track_id(), Some("t2"));
    let first_clip = s.practice().vocal_tracks[0].clips[0].id.clone();
    s.act(|vm| vm.select_clip("t2", &first_clip));
    s.act(|vm| vm.set_play_reference(false));
    assert_eq!(s.vm.selected_track_id(), Some("t2"), "the user chose there");
    assert_eq!(s.vm.selected_clip_id(), Some(first_clip.as_str()));

    s.act(|vm| vm.select_clip("acc", "acc-c"));
    s.act(|vm| vm.set_play_reference(false)); // the shown segment, tapped again
    assert_eq!(s.vm.selected_track_id(), Some("acc"));
    assert_eq!(s.vm.selected_clip_id(), Some("acc-c"));
}
