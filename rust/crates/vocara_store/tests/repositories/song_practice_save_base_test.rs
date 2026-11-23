//! Port of the repository cases of
//! `client/test/song_practice_save_base_test.dart`: the base a queued save
//! merges against, on the real store.
//!
//! When a second edit lands while a save is in flight, the view model keeps
//! its newer model rather than adopting the re-read aggregate, and merges it
//! against the save's SENT hashes ([`Loaded::sent_hashes`]). It used to adopt
//! the re-read's FRESH base anyway — but that base includes rows the
//! three-way rules kept from a concurrent writer, rows the newer model has
//! never seen. The queued save then merged a stale model against a base that
//! knew about them: a remote insert was tombstoned (in base, not desired), a
//! remote edit reverted (mine != base) and a remote delete resurrected.
//!
//! The Dart cases that drive the detail view model (its save queue, toasts,
//! engine resyncs, lyrics saves) belong to the view model's port; here the
//! queued save is played with the repository calls the view model makes.

use std::sync::Arc;

use vocara_model::models::audio_fx::{ReverbSettings, TrackFx, TrackSend};
use vocara_model::models::audio_track::{AudioClip, AudioTrack, AudioTrackKind};
use vocara_model::models::fx_bus::FxBus;
use vocara_model::models::lyric_line::LyricLine;
use vocara_model::models::practice_note::PracticeNote;
use vocara_model::models::song_practice::{same_parts, SongPractice};
use vocara_model::models::take_version::TakeVersion;
use vocara_model::util::id::new_id;
use vocara_store::repositories::SongPracticeRepository;
use vocara_store::sync::sync_hashes::Loaded;

use super::{arcs, d2026, lane, note, take, with_takes, FailNextLoad, Fixture};

/// A note at 10 ms.
fn note10(content: &str) -> PracticeNote {
    PracticeNote {
        position_ms: 10,
        ..note(content)
    }
}

/// A practice with one take carrying [notes], persisted, as loaded.
fn seed(f: &Fixture, notes: Vec<PracticeNote>) -> SongPractice {
    let created = f.create("Song");
    let take = AudioTrack {
        notes,
        ..take("Take 1", created.active_version_id.as_deref())
    };
    f.save(&with_takes(&created, vec![take]));
    f.load(&created.id).aggregate
}

/// Another writer (the sync applier, another editor) changes the take's
/// notes while the screen holds the aggregate.
fn concurrently(f: &Fixture, id: &str, edit: impl Fn(&[PracticeNote]) -> Vec<PracticeNote>) {
    let other = f.load(id);
    let take = &other.aggregate.vocal_tracks[0];
    let edited = AudioTrack {
        notes: edit(&take.notes),
        ..(**take).clone()
    };
    f.save_with_base(
        &with_takes(&other.aggregate, vec![edited]),
        &other.base_hashes,
    )
    .unwrap();
}

/// [p] with its one take renamed [name].
fn renamed(p: &SongPractice, name: &str) -> SongPractice {
    with_takes(
        p,
        vec![AudioTrack {
            display_name: name.to_string(),
            ..(*p.vocal_tracks[0]).clone()
        }],
    )
}

/// Two quick local edits on the aggregate the screen [held], the second
/// queued while the first save ran: the screen's model moves on from the one
/// the first save sent, so the second save sends that newer model against
/// the first save's SENT hashes.
fn rename_twice(f: &Fixture, held: &Loaded<SongPractice>) {
    let first_model = renamed(&held.aggregate, "A");
    let first = f.save_with_base(&first_model, &held.base_hashes).unwrap();
    let second_model = renamed(&first_model, "B");
    f.save_with_base(&second_model, first.sent_hashes.as_ref().unwrap())
        .unwrap();
}

#[test]
fn a_concurrent_insert_survives_the_queued_save() {
    let f = Fixture::new();
    let p = seed(&f, Vec::new());
    let held = f.load(&p.id);
    let remote = note10("from the other device");
    concurrently(&f, &p.id, |notes| {
        let mut out = notes.to_vec();
        out.push(remote.clone());
        out
    });

    rename_twice(&f, &held);

    let take = f.load(&p.id).aggregate.vocal_tracks[0].clone();
    assert_eq!(take.display_name, "B");
    assert_eq!(
        take.notes.iter().map(|n| n.id.clone()).collect::<Vec<_>>(),
        vec![remote.id.clone()]
    );
}

#[test]
fn a_concurrent_edit_survives_the_queued_save() {
    let f = Fixture::new();
    let p = seed(&f, vec![note10("as loaded")]);
    let held = f.load(&p.id);
    concurrently(&f, &p.id, |notes| {
        vec![PracticeNote {
            content: "edited elsewhere".into(),
            ..notes[0].clone()
        }]
    });

    rename_twice(&f, &held);

    let take = f.load(&p.id).aggregate.vocal_tracks[0].clone();
    assert_eq!(take.display_name, "B");
    assert_eq!(take.notes[0].content, "edited elsewhere");
}

#[test]
fn a_concurrent_delete_stays_deleted_after_the_queued_save() {
    let f = Fixture::new();
    let p = seed(&f, vec![note10("to be deleted elsewhere")]);
    let held = f.load(&p.id);
    concurrently(&f, &p.id, |_| Vec::new());

    rename_twice(&f, &held);

    let take = f.load(&p.id).aggregate.vocal_tracks[0].clone();
    assert_eq!(take.display_name, "B");
    assert!(take.notes.is_empty());
}

#[test]
fn a_save_whose_reread_fails_still_reports_what_it_committed() {
    // The write had committed, but the failed re-read came back as a failed
    // save: the caller reverted to a store that held it, or took its change
    // back over it (a separation's undo then collided with its own backing).
    let f = Fixture::new();
    let p = seed(&f, Vec::new());
    let store = f.other_songs();
    let fail = FailNextLoad::default();
    store.set_debug_load_hook(Some(fail.hook()));
    let loaded = f.load(&p.id);
    let renamed = renamed(&loaded.aggregate, "renamed");
    fail.arm();

    let saved = store
        .save_practice_with_base(&renamed, &loaded.base_hashes)
        .unwrap();

    // The aggregate IS the one sent (Dart: identical), its lanes the very
    // same parts.
    assert!(!saved.reread);
    assert_eq!(saved.aggregate, renamed);
    assert!(same_parts(
        &saved.aggregate.vocal_tracks,
        &renamed.vocal_tracks
    ));
    let after = f.load(&p.id);
    assert_eq!(after.aggregate.vocal_tracks[0].display_name, "renamed");
    // With nothing concurrent, the base it stands in is a re-read's exactly.
    assert_eq!(saved.base_hashes, after.base_hashes);
}

#[test]
fn a_save_reports_what_it_sent_in_the_same_hashes_a_load_reports() {
    // With nothing concurrent, the sent state IS the stored state, so the two
    // maps must agree key for key: a save-side projection that drifted from
    // the row hash would read every untouched row as edited.
    let f = Fixture::new();
    let p = seed(&f, vec![note10("n")]);
    let loaded = f.load(&p.id);
    let take = &loaded.aggregate.vocal_tracks[0];
    let full = SongPractice {
        lyrics: vec![LyricLine::new("la", Some(100))],
        reverb: Some(ReverbSettings::DEFAULT),
        fx_buses: arcs(vec![FxBus::new(new_id(), "Bus", TrackFx::flat())]),
        vocal_tracks: arcs(vec![AudioTrack {
            fx: Some(TrackFx::flat()),
            technique_markers: vec![super::marker(5, "Twang")],
            ..(**take).clone()
        }]),
        ..loaded.aggregate.clone()
    };
    let saved = f.save_with_base(&full, &loaded.base_hashes).unwrap();
    assert_eq!(saved.sent_hashes, Some(saved.base_hashes.clone()));
}

#[test]
fn for_every_entity_a_practice_can_carry_and_again_on_a_save_from_the_reread_pool_paths() {
    // The test above has no clips, backings, separation source, second
    // interpretation, sends or bus output routing: a projection drift in any
    // of those would make every save (a lyric stamp included) re-read the
    // practice and resync the engine.
    let f = Fixture::new();
    let file = |name: &str, seed: u32| {
        let bytes: Vec<u8> = (0..200u32).map(|i| ((i * seed) & 0xff) as u8).collect();
        f.file(name, &bytes)
    };

    let p = seed(&f, Vec::new());
    let loaded = f.load(&p.id);
    let v2 = TakeVersion::new(new_id(), "Interp 2", d2026());
    let bus = FxBus::new(new_id(), "Bus", TrackFx::flat());
    let bus2 = FxBus {
        output_bus_id: Some(bus.id.clone()),
        sends: vec![TrackSend::new(bus.id.clone(), -6.0)],
        ..FxBus::new(new_id(), "Bus2", TrackFx::flat())
    };
    let track = |kind: AudioTrackKind, path: String, version: Option<&str>| {
        let vocal = kind == AudioTrackKind::Vocal;
        AudioTrack {
            clips: vec![AudioClip {
                start_ms: 10,
                waveform_peaks: vec![0.1, 0.2],
                ..AudioClip::new(new_id(), path, 1000)
            }],
            version_id: version.map(str::to_string),
            bus_id: vocal.then(|| bus.id.clone()),
            sends: if vocal {
                vec![TrackSend::new(bus2.id.clone(), -3.0)]
            } else {
                Vec::new()
            },
            fx: vocal.then(TrackFx::flat),
            notes: vec![note10("n")],
            ..lane(kind, kind.name())
        }
    };

    let mut versions = loaded.aggregate.versions.clone();
    versions.push(Arc::new(v2.clone()));
    let full = SongPractice {
        versions,
        accompaniment: Some(Arc::new(track(
            AudioTrackKind::Accompaniment,
            file("acc.wav", 3),
            None,
        ))),
        reference: Some(Arc::new(track(
            AudioTrackKind::Reference,
            file("ref.wav", 5),
            None,
        ))),
        vocal_tracks: arcs(vec![
            track(
                AudioTrackKind::Vocal,
                file("v1.wav", 7),
                p.active_version_id.as_deref(),
            ),
            track(AudioTrackKind::Vocal, file("v2.wav", 11), Some(&v2.id)),
            // The same bytes as v1: a deduplicated asset.
            track(AudioTrackKind::Vocal, file("v1dup.wav", 7), Some(&v2.id)),
        ]),
        fx_buses: arcs(vec![bus.clone(), bus2.clone()]),
        separation_source: Some(file("orig.wav", 13)),
        reverb: Some(ReverbSettings::DEFAULT),
        lyrics: vec![LyricLine::new("la", Some(1))],
        ..loaded.aggregate.clone()
    };
    let first = f.save_with_base(&full, &loaded.base_hashes).unwrap();
    assert_eq!(
        first.sent_hashes,
        Some(first.base_hashes.clone()),
        "first save"
    );

    let second = f
        .save_with_base(
            &SongPractice {
                lyrics: vec![LyricLine::new("lb", Some(2))],
                ..first.aggregate.clone()
            },
            &first.base_hashes,
        )
        .unwrap();
    assert_eq!(
        second.sent_hashes,
        Some(second.base_hashes.clone()),
        "second save"
    );
}
