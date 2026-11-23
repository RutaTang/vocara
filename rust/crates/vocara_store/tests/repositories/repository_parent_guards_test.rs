//! Port of `client/test/repository_parent_guards_test.dart`.
//!
//! The practice and technique repositories answer a deleted or missing
//! parent alike: a delete of one writes nothing, and a save over one fails
//! before importing any file.

use indexmap::IndexMap;
use vocara_model::models::audio_track::{AudioClip, AudioTrack};
use vocara_model::models::song_practice::SongPractice;
use vocara_model::models::technique::Technique;
use vocara_model::models::technique_media::{TechniqueMedia, TechniqueMediaKind};
use vocara_model::util::id::new_id;
use vocara_store::repositories::{SongPracticeRepository, TechniqueRepository};
use vocara_store::sync::sync_hashes::Loaded;

use super::{d2026, take, with_takes, Fixture};

/// Writes [bytes] to `root/staging/<name>`.
fn file(f: &Fixture, name: &str, bytes: &[u8]) -> String {
    f.file(&format!("staging/{name}"), bytes)
}

fn seed_practice(f: &Fixture) -> Loaded<SongPractice> {
    let created = f.create("S");
    let take = AudioTrack {
        clips: vec![AudioClip::new(new_id(), file(f, "a.wav", &[1, 2, 3]), 1000)],
        ..take("Take", Some(&created.versions[0].id))
    };
    let loaded = f.load(&created.id);
    f.save_with_base(
        &with_takes(&loaded.aggregate, vec![take]),
        &loaded.base_hashes,
    )
    .unwrap()
}

fn seed_technique(f: &Fixture) -> Loaded<Technique> {
    let t = Technique {
        media: vec![TechniqueMedia {
            id: new_id(),
            path: file(f, "m.wav", &[4, 5, 6]),
            kind: TechniqueMediaKind::Audio,
            label: String::new(),
            added_at: d2026(),
        }],
        ..Technique::new(new_id(), "Twang", d2026(), d2026())
    };
    f.techs
        .save_technique_with_base(&t, &IndexMap::new())
        .unwrap()
}

#[test]
fn deleting_a_deleted_or_unknown_parent_writes_nothing() {
    // Tombstoned again, it journaled a fresh delete (for a row that never
    // existed, too).
    let f = Fixture::new();
    let p0 = seed_practice(&f);
    let t0 = seed_technique(&f);
    f.songs.delete_practice(&p0.aggregate.id).unwrap();
    f.techs.delete_technique(&t0.aggregate.id).unwrap();
    let before = f.ops();
    f.songs.delete_practice(&p0.aggregate.id).unwrap();
    f.techs.delete_technique(&t0.aggregate.id).unwrap();
    f.songs.delete_practice(&new_id()).unwrap();
    f.techs.delete_technique(&new_id()).unwrap();
    assert_eq!(f.ops(), before);
}

#[test]
fn a_save_over_a_deleted_parent_imports_nothing() {
    // Refused only inside the row transaction, its new files had already
    // been imported and journaled for a save that then failed.
    let f = Fixture::new();
    let p0 = seed_practice(&f);
    let t0 = seed_technique(&f);
    f.songs.delete_practice(&p0.aggregate.id).unwrap();
    f.techs.delete_technique(&t0.aggregate.id).unwrap();
    let before = f.ops();
    let held = &p0.aggregate.vocal_tracks[0];
    let mut clips = held.clips.clone();
    clips.push(AudioClip {
        start_ms: 2000,
        ..AudioClip::new(new_id(), file(&f, "new.wav", &[7, 8, 9]), 500)
    });
    let practice_save = f.save_with_base(
        &with_takes(
            &p0.aggregate,
            vec![AudioTrack {
                clips,
                ..(**held).clone()
            }],
        ),
        &p0.base_hashes,
    );
    let mut media = t0.aggregate.media.clone();
    media.push(TechniqueMedia {
        id: new_id(),
        path: file(&f, "new.m4a", &[10, 11, 12]),
        kind: TechniqueMediaKind::Audio,
        label: String::new(),
        added_at: d2026(),
    });
    let technique_save = f.techs.save_technique_with_base(
        &Technique {
            media,
            ..t0.aggregate.clone()
        },
        &t0.base_hashes,
    );
    assert!(practice_save.is_err());
    assert!(technique_save.is_err());
    assert_eq!(f.ops(), before);
}

#[test]
fn a_stored_time_past_what_a_date_time_holds_still_lists_and_opens() {
    // Every list of the library threw on one such row, and showed nothing.
    let f = Fixture::new();
    let p0 = seed_practice(&f);
    let t0 = seed_technique(&f);
    f.raw(
        &format!(
            "UPDATE practices SET updated_at = {}, created_at = {}",
            1i64 << 62,
            -(1i64 << 62)
        ),
        [],
    );
    f.raw(
        &format!("UPDATE techniques SET updated_at = {}", 1i64 << 62),
        [],
    );

    let listed = f.songs.list_practice_summaries().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, p0.aggregate.id);
    assert!(f.songs.load_practice(&p0.aggregate.id).is_ok());
    let techniques = f.techs.list_techniques().unwrap();
    assert_eq!(techniques.len(), 1);
    assert_eq!(techniques[0].id, t0.aggregate.id);
}

#[test]
fn a_stored_level_or_pan_past_the_faders_loads_within_them() {
    // A peer's level went to the engine verbatim: 1e30 a gain.
    let f = Fixture::new();
    let p0 = seed_practice(&f);
    f.raw("UPDATE tracks SET volume = 1e30, pan = -7.5", []);
    f.raw("UPDATE practices SET master_volume = 1e30", []);
    let loaded = f.load(&p0.aggregate.id).aggregate;
    assert_eq!(loaded.master_volume, 1.0);
    for t in &loaded.vocal_tracks {
        assert_eq!(t.volume, 1.0);
        assert_eq!(t.pan, -1.0);
    }
}
