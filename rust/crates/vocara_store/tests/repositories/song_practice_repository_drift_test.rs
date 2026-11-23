//! Port of `client/test/song_practice_repository_drift_test.dart`.

use std::fs;
use std::sync::Arc;

use serde_json::json;
use vocara_model::models::audio_fx::{CompSettings, ReverbSettings, TrackFx, TrackSend};
use vocara_model::models::audio_track::{AudioClip, AudioTrack, AudioTrackKind};
use vocara_model::models::fx_bus::FxBus;
use vocara_model::models::lyric_line::LyricLine;
use vocara_model::models::practice_note::PracticeNote;
use vocara_model::models::practice_summary::PracticeSummary;
use vocara_model::models::song_practice::SongPractice;
use vocara_model::models::take_version::TakeVersion;
use vocara_model::models::technique_marker::TechniqueMarker;
use vocara_model::util::dart_date_time::DateTime;
use vocara_model::util::dart_json;
use vocara_model::util::id::new_id;
use vocara_store::db::rows::{OplogRow, PracticeRow};
use vocara_store::repositories::SongPracticeRepository;
use vocara_store::sync::hlc::{Hlc, HlcClock};
use vocara_store::sync::integrity_checker::IntegrityChecker;
use vocara_store::sync::quarantine_store::{QuarantineEntry, QuarantineStore};
use vocara_store::sync::sync_hashes::Loaded;
use vocara_store::sync::sync_state_store::SyncStateStore;
use vocara_store::sync::sync_writer::SyncWriter;

use super::{arcs, d2026, lane, note, pad_id, take, with_takes, Fixture};

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn create_practice_seeds_exactly_one_interpretation() {
    let f = Fixture::new();
    let pr = f.create("My Song");
    assert_eq!(pr.title, "My Song");
    assert_eq!(pr.versions.len(), 1);
    assert_eq!(
        pr.active_version_id.as_deref(),
        Some(pr.versions[0].id.as_str())
    );
}

#[test]
fn every_editable_field_round_trips_no_column_silently_dropped() {
    // The narrower round-trip below pins a representative subset. This one
    // sets every user-editable field on every row type to a distinct
    // non-default value, so a column the writer forgets — or the hydrator
    // never reads — shows up as a lost edit here instead of as "my pan reset
    // itself".
    let f = Fixture::new();
    let base = f.create("Everything");
    let version_id = base.versions[0].id.clone();
    let bus_a = new_id();
    let bus_b = new_id();
    let original_mix = f.file("original-mix.wav", &[4, 2]);

    let fx = TrackFx {
        eq_on: true,
        comp_on: true,
        de_esser_on: true,
        send_db: -6.0,
        comp: CompSettings {
            threshold_db: -18.0,
            ratio: 3.5,
            makeup_db: 2.0,
            ..CompSettings::DEFAULT
        },
        ..TrackFx::flat()
    };
    let vocal = AudioTrack {
        version_id: Some(version_id.clone()),
        volume: 0.63,
        pan: -0.42,
        muted: true,
        soloed: true,
        color_value: Some(0xFF123456),
        bus_id: Some(bus_a.clone()),
        fx: Some(fx.clone()),
        sends: vec![TrackSend::new(bus_b.clone(), -9.0)],
        clips: vec![
            AudioClip {
                start_ms: 250,
                ..f.clip_with_peaks("a.wav", &[1], &[0.25])
            },
            AudioClip {
                start_ms: 9000,
                ..f.clip_with_peaks("b.wav", &[2], &[0.5])
            },
        ],
        notes: vec![PracticeNote {
            id: new_id(),
            position_ms: 300,
            end_ms: Some(1300),
            content: "ranged note".into(),
            created_at: d2026(),
            updated_at: d2026(),
        }],
        technique_markers: vec![TechniqueMarker {
            id: new_id(),
            position_ms: 400,
            end_ms: Some(1400),
            technique_id: "tech-9".into(),
            technique_name: "Belt".into(),
            created_at: d2026(),
            updated_at: d2026(),
        }],
        ..lane(AudioTrackKind::Vocal, "Everything Take")
    };

    let saved = f.save(&SongPractice {
        vocal_tracks: arcs(vec![vocal]),
        timeline_duration_ms: 44000,
        master_volume: 0.71,
        separation_source: Some(original_mix),
        reverb: Some(ReverbSettings {
            room_size: 0.81,
            damping: 0.22,
            wet: 0.33,
            width: 0.44,
        }),
        fx_buses: arcs(vec![
            FxBus {
                color_value: Some(0xFF654321),
                volume: 0.55,
                muted: true,
                soloed: true,
                pan: 0.37,
                output_bus_id: Some(bus_b.clone()),
                sends: vec![TrackSend::new(bus_b.clone(), -12.0)],
                ..FxBus::new(bus_a.clone(), "Lead", fx.clone())
            },
            FxBus::new(bus_b.clone(), "Verb", TrackFx::flat()),
        ]),
        ..base.clone()
    });
    assert_eq!(saved.id, base.id);

    let l = f.songs.get_practice(&base.id).unwrap();
    assert!(close(l.master_volume, 0.71));
    assert_eq!(l.timeline_duration_ms, 44000);
    let separation = l.separation_source.clone().expect("a separation source");
    assert_eq!(fs::read(&separation).unwrap(), vec![4, 2]);
    let reverb = l.reverb.unwrap();
    assert!(close(reverb.room_size, 0.81));
    assert!(close(reverb.damping, 0.22));
    assert!(close(reverb.wet, 0.33));
    assert!(close(reverb.width, 0.44));

    let v = &l.vocal_tracks[0];
    assert_eq!(l.vocal_tracks.len(), 1);
    assert!(close(v.volume, 0.63));
    assert!(close(v.pan, -0.42));
    assert!(v.muted);
    assert!(v.soloed);
    assert_eq!(v.color_value, Some(0xFF123456));
    assert_eq!(v.bus_id.as_deref(), Some(bus_a.as_str()));
    assert_eq!(v.sends.len(), 1);
    assert_eq!(v.sends[0].bus_id, bus_b);
    assert!(close(v.sends[0].level_db, -9.0));
    let vfx = v.fx.as_ref().unwrap();
    assert!(vfx.eq_on);
    assert!(vfx.comp_on);
    assert!(vfx.de_esser_on);
    assert!(close(vfx.send_db, -6.0));
    assert!(close(vfx.comp.threshold_db, -18.0));
    assert_eq!(
        v.clips.iter().map(|c| c.start_ms).collect::<Vec<_>>(),
        vec![250, 9000]
    );
    assert_eq!(v.notes[0].end_ms, Some(1300));
    assert_eq!(v.technique_markers[0].end_ms, Some(1400));

    let a = l.fx_buses.iter().find(|b| b.id == bus_a).unwrap();
    assert_eq!(a.name, "Lead");
    assert_eq!(a.color_value, Some(0xFF654321));
    assert!(close(a.volume, 0.55));
    assert!(a.muted);
    assert!(a.soloed);
    assert!(close(a.pan, 0.37));
    assert_eq!(a.output_bus_id.as_deref(), Some(bus_b.as_str()));
    assert_eq!(a.sends[0].bus_id, bus_b);
    assert!(close(a.sends[0].level_db, -12.0));
    assert!(a.fx.eq_on);
}

#[test]
fn full_graph_round_trips_through_save_then_load() {
    let f = Fixture::new();
    let base = f.create("Hai Kuo");
    let version_id = base.versions[0].id.clone();

    let accomp = AudioTrack {
        clips: vec![f.clip("backing.wav", &[1, 2, 3])],
        ..lane(AudioTrackKind::Accompaniment, "Backing")
    };
    let vocal = AudioTrack {
        volume: 0.7,
        muted: true,
        clips: vec![f.clip_with_peaks("take.wav", &[9, 8, 7], &[0.5, 0.25, 0.75])],
        notes: vec![PracticeNote {
            position_ms: 1200,
            ..note("breathy onset")
        }],
        technique_markers: vec![TechniqueMarker {
            technique_id: "tech-1".into(),
            ..super::marker(2000, "Twang")
        }],
        ..take("Take 1", Some(&version_id))
    };
    let bus = FxBus::new(new_id(), "Reverb Bus", TrackFx::flat());

    f.save(&SongPractice {
        accompaniment: Some(Arc::new(accomp)),
        vocal_tracks: arcs(vec![vocal]),
        lyrics: vec![LyricLine::new("hai kuo tian kong", Some(0))],
        timeline_duration_ms: 30000,
        master_volume: 0.8,
        fx_buses: arcs(vec![bus]),
        ..base.clone()
    });

    let loaded = f.songs.get_practice(&base.id).unwrap();
    assert_eq!(loaded.master_volume, 0.8);
    assert_eq!(loaded.timeline_duration_ms, 30000);
    assert_eq!(
        loaded.active_version_id.as_deref(),
        Some(version_id.as_str())
    );
    assert_eq!(loaded.lyrics[0].text, "hai kuo tian kong");
    assert_eq!(loaded.fx_buses[0].name, "Reverb Bus");

    // Accompaniment + its clip resolve to a readable pooled file.
    let accompaniment = loaded.accompaniment.as_ref().unwrap();
    assert_eq!(
        fs::read(&accompaniment.clips[0].audio_file_path).unwrap(),
        vec![1, 2, 3]
    );

    // Vocal track + children.
    assert_eq!(loaded.vocal_tracks.len(), 1);
    let v = &loaded.vocal_tracks[0];
    assert_eq!(v.version_id.as_deref(), Some(version_id.as_str()));
    assert_eq!(v.volume, 0.7);
    assert!(v.muted);
    assert_eq!(v.notes[0].content, "breathy onset");
    assert_eq!(v.technique_markers[0].technique_name, "Twang");
    // Waveform peaks survive (cached by content hash).
    assert_eq!(v.clips[0].waveform_peaks, vec![0.5, 0.25, 0.75]);
}

#[test]
fn editing_the_title_bumps_updated_at_and_an_unchanged_save_is_a_no_op() {
    let f = Fixture::new();
    let v1 = f.create("A");
    let before_ops = f.oplog().len();

    let v1b = f.save(&v1); // identical
    assert_eq!(v1b.updated_at, v1.updated_at);
    assert_eq!(f.oplog().len(), before_ops);

    std::thread::sleep(std::time::Duration::from_millis(2));
    let v2 = f.save(&SongPractice {
        title: "A renamed".into(),
        ..v1.clone()
    });
    assert_eq!(v2.title, "A renamed");
    assert!(v2.updated_at.is_after(&v1.updated_at));
}

#[test]
fn removing_a_vocal_track_cascade_tombstones_its_clips_notes_markers() {
    let f = Fixture::new();
    let base = f.create("S");
    let vocal = AudioTrack {
        clips: vec![f.clip("t.wav", &[1])],
        notes: vec![note("x")],
        ..take("Take", Some(&base.versions[0].id))
    };
    f.save(&with_takes(&base, vec![vocal]));
    assert_eq!(f.live_count("clips"), 1);
    assert_eq!(f.live_count("practice_notes"), 1);

    // Save without the vocal track.
    f.save(&base);
    let loaded = f.songs.get_practice(&base.id).unwrap();
    assert!(loaded.vocal_tracks.is_empty());
    assert_eq!(f.live_count("tracks"), 0);
    assert_eq!(f.live_count("clips"), 0); // cascaded
    assert_eq!(f.live_count("practice_notes"), 0);
}

#[test]
fn the_summary_read_reports_what_summarizing_the_full_list_would() {
    // Home reads summaries instead of hydrating every practice (every clip's
    // peaks, a file check per clip) on each return to it; the counts must
    // still be exactly what the hydrated aggregate gives.
    let f = Fixture::new();
    let track = |kind: AudioTrackKind, version: Option<&str>, notes: usize| AudioTrack {
        version_id: version.map(str::to_string),
        notes: (0..notes).map(|_| note("n")).collect(),
        ..lane(kind, kind.name())
    };
    let empty = f.create("Empty");
    let full = f.create("Full");
    let v2 = TakeVersion::new(new_id(), "Second", d2026());
    let doomed = track(AudioTrackKind::Vocal, None, 3);
    let mut versions = full.versions.clone();
    versions.push(Arc::new(v2.clone()));
    let saved = f.save(&SongPractice {
        versions,
        accompaniment: Some(Arc::new(track(AudioTrackKind::Accompaniment, None, 1))),
        reference: Some(Arc::new(track(AudioTrackKind::Reference, None, 2))),
        vocal_tracks: arcs(vec![
            track(AudioTrackKind::Vocal, full.active_version_id.as_deref(), 0),
            track(AudioTrackKind::Vocal, Some(&v2.id), 4),
            doomed.clone(),
        ]),
        ..full.clone()
    });
    // A tombstoned take (and its notes) and a tombstoned note count for
    // neither; nor does a deleted practice.
    f.raw(
        "UPDATE tracks SET deleted_at = 1 WHERE id = ?",
        [&doomed.id],
    );
    f.raw(
        "UPDATE practice_notes SET deleted_at = 1 WHERE id = ?",
        [&saved.vocal_tracks[1].notes[0].id],
    );
    let gone = f.create("Gone");
    f.songs.delete_practice(&gone.id).unwrap();

    let show = |p: &PracticeSummary| {
        format!(
            "{} {} {} {} {} {}",
            p.id,
            p.title,
            p.updated_at.millis_since_epoch(),
            p.take_count,
            p.has_accompaniment,
            p.note_count
        )
    };
    let hydrated: Vec<String> = f
        .songs
        .list_practices()
        .unwrap()
        .iter()
        .map(|p| show(&PracticeSummary::of(p)))
        .collect();
    let summaries = f.songs.list_practice_summaries().unwrap();
    assert_eq!(summaries.iter().map(show).collect::<Vec<_>>(), hydrated);
    assert_eq!(
        summaries.iter().map(|p| p.id.clone()).collect::<Vec<_>>(),
        vec![saved.id.clone(), empty.id.clone()]
    );
    assert!(show(&summaries[0]).ends_with(" 2 true 6"));

    // One hydration cannot read is still LISTED (deliberately: it can be
    // deleted from Home), where the hydrated list drops it.
    let corrupt = f.create("Corrupt");
    f.raw(
        "UPDATE practices SET lyrics_json = '{not json' WHERE id = ?",
        [&corrupt.id],
    );
    let listed: Vec<String> = f
        .songs
        .list_practices()
        .unwrap()
        .into_iter()
        .map(|p| p.id)
        .collect();
    assert!(!listed.contains(&corrupt.id));
    let summarized: Vec<String> = f
        .songs
        .list_practice_summaries()
        .unwrap()
        .into_iter()
        .map(|p| p.id)
        .collect();
    assert!(summarized.contains(&corrupt.id));
}

#[test]
fn delete_tombstones_the_practice_and_the_list_excludes_it() {
    let f = Fixture::new();
    let a = f.create("A");
    f.create("B");
    assert_eq!(f.songs.list_practices().unwrap().len(), 2);

    f.songs.delete_practice(&a.id).unwrap();
    assert_eq!(f.songs.list_practices().unwrap().len(), 1);
    assert!(f.songs.get_practice(&a.id).is_err());
}

/// A stamp a minute ahead of this clock, from another node.
fn ahead() -> String {
    Hlc::new(
        DateTime::now().millis_since_epoch() + 60000,
        7,
        pad_id("OTHER"),
    )
    .packed()
}

#[test]
fn an_edit_or_delete_over_a_row_stamped_ahead_of_this_clock_orders_after_it() {
    // A second running instance shares the library with a clock of its own,
    // so a row it wrote can carry a stamp ahead of ours. Stamped from our
    // clock alone, the edit replacing it lost entity-level LWW to the very
    // version it replaced.
    let f = Fixture::new();
    let pr = f.create("Mine");
    let version_id = pr.versions[0].id.clone();
    let ahead = ahead();
    let plant = |table: &str, id: &str| {
        f.raw(
            &format!("UPDATE {table} SET hlc_packed = ? WHERE id = ?"),
            [ahead.as_str(), id],
        )
    };
    let held = |table: &str, id: &str| f.column::<String>(table, "hlc_packed", id);

    plant("practices", &pr.id);
    plant("interpretations", &version_id);
    let saved = f.save(&SongPractice {
        title: "Renamed".into(),
        versions: pr
            .versions
            .iter()
            .map(|v| {
                Arc::new(TakeVersion {
                    name: "Renamed take".into(),
                    ..(**v).clone()
                })
            })
            .collect(),
        ..pr.clone()
    });
    assert_eq!(saved.title, "Renamed");
    assert!(held("practices", &pr.id) > ahead);
    assert!(held("interpretations", &version_id) > ahead);

    plant("practices", &pr.id);
    plant("interpretations", &version_id);
    f.songs.delete_practice(&pr.id).unwrap();
    assert!(held("practices", &pr.id) > ahead);
    assert!(held("interpretations", &version_id) > ahead);
    // The journal carries the same order.
    let last: Vec<OplogRow> = f.db.select("entity_id = ?1", [&pr.id]).unwrap();
    assert!(last.last().unwrap().hlc_packed > ahead);
}

#[test]
fn an_edit_orders_after_the_high_water_mark_of_accepted_remote_stamps() {
    // A second instance that parked a newer state of this row raised the
    // mark, not the row: our edit must still out-rank that parked state.
    let f = Fixture::new();
    let pr = f.create("Mine");
    let ahead = ahead();
    SyncStateStore::new(f.db.clone())
        .raise_hlc_high_water(&ahead)
        .unwrap();
    f.save(&SongPractice {
        title: "Renamed".into(),
        ..pr.clone()
    });
    assert!(f.column::<String>("practices", "hlc_packed", &pr.id) > ahead);
}

// --- regression: data-loss blockers ---

#[test]
fn a_clip_whose_pooled_file_goes_missing_survives_reload_and_resave() {
    // Never dropped or tombstoned.
    let f = Fixture::new();
    let base = f.create("S");
    let vocal = AudioTrack {
        clips: vec![f.clip("take.wav", &[1, 2, 3])],
        ..take("Take", Some(&base.versions[0].id))
    };
    f.save(&with_takes(&base, vec![vocal]));

    // Simulate the bytes going missing locally.
    let rel: String =
        f.db.query_row("SELECT local_rel_path FROM assets LIMIT 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    fs::remove_file(f.root().join(rel)).unwrap();

    // Reload keeps the clip (as an asset:// ref) — it must NOT be dropped.
    let reloaded = f.songs.get_practice(&base.id).unwrap();
    assert_eq!(reloaded.vocal_tracks[0].clips.len(), 1);
    assert!(reloaded.vocal_tracks[0].clips[0]
        .audio_file_path
        .starts_with("asset://"));

    // Re-saving the reloaded model must NOT tombstone the still-valid clip
    // row.
    f.save(&reloaded);
    assert_eq!(f.live_count("clips"), 1);
}

#[test]
fn a_save_that_touches_no_audio_keeps_every_clip_on_its_asset_even_when_a_pooled_file_changed() {
    let f = Fixture::new();
    let base = f.create("S");
    let vocal = AudioTrack {
        clips: vec![f.clip("take.wav", &[1, 2, 3, 4])],
        ..take("Take", Some(&base.versions[0].id))
    };
    f.save(&with_takes(&base, vec![vocal]));
    let reloaded = f.songs.get_practice(&base.id).unwrap();
    let asset_before =
        f.column::<String>("clips", "asset_id", &reloaded.vocal_tracks[0].clips[0].id);

    // The clip comes back carrying its pool path; flip a byte under it (rot /
    // an outside edit — verify_pool_integrity's report-only case).
    let pooled = &reloaded.vocal_tracks[0].clips[0].audio_file_path;
    fs::write(pooled, [1, 2, 3, 5]).unwrap();

    let ops_before = f.oplog().len();
    f.save(&SongPractice {
        title: "Renamed".into(),
        ..reloaded.clone()
    });

    // A rename must not mint an asset for the changed bytes and re-point the
    // clip at it (journaled, that would ship the bad bytes fleet-wide).
    assert_eq!(
        f.column::<String>("clips", "asset_id", &reloaded.vocal_tracks[0].clips[0].id),
        asset_before
    );
    assert_eq!(f.count("SELECT COUNT(*) FROM assets", []), 1);
    assert_eq!(f.ops()[ops_before..], ["practices:update".to_string()]);
}

/// `SongPractice(id:, title:, createdAt:, updatedAt:, accompaniment:,
/// reference:, versions:, activeVersionId:)` over [base].
fn with_slots(
    base: &SongPractice,
    accompaniment: Option<AudioTrack>,
    reference: Option<AudioTrack>,
) -> SongPractice {
    SongPractice {
        accompaniment: accompaniment.map(Arc::new),
        reference: reference.map(Arc::new),
        vocal_tracks: Vec::new(),
        ..base.clone()
    }
}

fn live_of_kind(f: &Fixture, kind: &str) -> i64 {
    f.count(
        "SELECT COUNT(*) FROM tracks WHERE kind = ? AND deleted_at IS NULL",
        [kind],
    )
}

#[test]
fn replacing_the_accompaniment_does_not_roll_back() {
    // Tombstone before insert frees the unique slot.
    let f = Fixture::new();
    let base = f.create("S");
    f.save(&with_slots(
        &base,
        Some(AudioTrack {
            clips: vec![f.clip("b1.wav", &[1])],
            ..lane(AudioTrackKind::Accompaniment, "B1")
        }),
        None,
    ));

    // Replace with a NEW accompaniment track (different id, same single
    // slot).
    let result = f.songs.save_practice(&with_slots(
        &base,
        Some(AudioTrack {
            clips: vec![f.clip("b2.wav", &[2])],
            ..lane(AudioTrackKind::Accompaniment, "B2")
        }),
        None,
    ));

    // Did NOT roll back on ux_track_slot.
    let saved = result.unwrap();
    assert_eq!(saved.accompaniment.as_ref().unwrap().display_name, "B2");
    assert_eq!(live_of_kind(&f, "accompaniment"), 1);
}

// Regression guard: the tombstone-first ordering only frees a slot when the
// old row LEAVES the aggregate. A slot ROTATION keeps the old row in desired
// with its kind changed — the first-separation flow re-kinds the original
// (accompaniment -> reference) while a fresh instrumental takes the
// accompaniment slot — and the new row's insert then collided with the old
// row still holding the slot in the DB, rolling back the whole save.
#[test]
fn slot_rotation_saves_without_a_ux_track_slot_rollback() {
    let f = Fixture::new();
    let base = f.create("S");
    let original_id = new_id();
    let original_clip = f.clip("mix.wav", &[1, 2, 3]);
    let original = |kind| AudioTrack {
        clips: vec![original_clip.clone()],
        ..AudioTrack::new(original_id.clone(), kind, "Full mix", d2026())
    };
    f.save(&with_slots(
        &base,
        Some(original(AudioTrackKind::Accompaniment)),
        None,
    ));

    // The separation result: same id, kind rotated to reference; a NEW id
    // claims the accompaniment slot in the same save.
    let result = f.songs.save_practice(&with_slots(
        &base,
        Some(AudioTrack {
            clips: vec![f.clip("inst.wav", &[4, 5])],
            ..lane(AudioTrackKind::Accompaniment, "Full mix (Instrumental)")
        }),
        Some(original(AudioTrackKind::Reference)),
    ));

    let loaded = result.unwrap();
    assert_eq!(
        loaded.accompaniment.as_ref().unwrap().display_name,
        "Full mix (Instrumental)"
    );
    assert_eq!(loaded.reference.as_ref().unwrap().id, original_id);
    for kind in ["accompaniment", "reference"] {
        assert_eq!(live_of_kind(&f, kind), 1, "exactly one live {kind} row");
    }
}

#[test]
fn a_symmetric_accompaniment_reference_swap_saves() {
    // No statement order can free both slots — the two-phase vacate must.
    let f = Fixture::new();
    let base = f.create("S");
    let a_id = new_id();
    let b_id = new_id();
    let a_clip = f.clip("a.wav", &[1]);
    let b_clip = f.clip("b.wav", &[2]);
    let track = |id: &str, kind, c: &AudioClip| AudioTrack {
        clips: vec![c.clone()],
        ..AudioTrack::new(id, kind, if id == a_id { "A" } else { "B" }, d2026())
    };
    f.save(&with_slots(
        &base,
        Some(track(&a_id, AudioTrackKind::Accompaniment, &a_clip)),
        Some(track(&b_id, AudioTrackKind::Reference, &b_clip)),
    ));

    // Swap the kinds between the SAME two live rows.
    let result = f.songs.save_practice(&with_slots(
        &base,
        Some(track(&b_id, AudioTrackKind::Accompaniment, &b_clip)),
        Some(track(&a_id, AudioTrackKind::Reference, &a_clip)),
    ));

    let loaded = result.unwrap();
    assert_eq!(loaded.accompaniment.as_ref().unwrap().id, b_id);
    assert_eq!(loaded.reference.as_ref().unwrap().id, a_id);
}

#[test]
fn removing_a_vocal_track_journals_exactly_one_tombstone_per_row_whose_hlc_matches_the_row() {
    // No duplicate / stale oplog entry.
    let f = Fixture::new();
    let base = f.create("S");
    let track = AudioTrack {
        clips: vec![f.clip("t.wav", &[1, 2])],
        ..take("Take", Some(&base.versions[0].id))
    };
    f.save(&with_takes(&base, vec![track.clone()]));

    // Save without the track -> reconcile-driven cascade tombstone.
    f.save(&base);

    // Exactly ONE tracks tombstone op for this id, and its HLC equals the
    // row's (the old bug emitted a second, HLC-mismatched entry).
    let track_ops: Vec<String> =
        f.db.query(
            "SELECT hlc_packed FROM oplog WHERE entity_table = 'tracks' \
             AND entity_id = ? AND op = 'tombstone'",
            [&track.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(track_ops.len(), 1);
    assert_eq!(
        track_ops[0],
        f.column::<String>("tracks", "hlc_packed", &track.id)
    );

    // The cascaded clip is tombstoned exactly once too.
    assert_eq!(
        f.count(
            "SELECT COUNT(*) FROM oplog WHERE entity_table = 'clips' AND op = 'tombstone'",
            []
        ),
        1
    );
}

// Regression guard: children were reconciled for EVERY desired track, even
// one whose remote delete the three-way merge had just accepted. A child the
// stale editor had edited was then revived (rule 5's edited side) as a live
// row under a tombstoned parent — invisible on this device, but synced out
// as an orphan.
#[test]
fn a_track_whose_remote_delete_is_accepted_takes_its_children_with_it() {
    let f = Fixture::new();
    let base = f.create("S");
    let vocal = AudioTrack {
        clips: vec![f.clip("t.wav", &[1])],
        notes: vec![note("x")],
        ..take("Take", Some(&base.versions[0].id))
    };
    f.save(&with_takes(&base, vec![vocal]));

    // An editor opens the practice: these hashes are its merge base.
    let loaded = f.load(&base.id);

    // The track is deleted elsewhere and syncs in, cascading to its
    // children.
    f.save(&base);
    assert_eq!(f.live_count("tracks"), 0);

    // The stale editor saves: it never touched the TRACK (so the delete is
    // accepted) but it did edit a note underneath it.
    let held = &loaded.aggregate.vocal_tracks[0];
    let edited = AudioTrack {
        notes: vec![PracticeNote {
            content: "edited while deleted".into(),
            ..held.notes[0].clone()
        }],
        ..(**held).clone()
    };
    let saved = f.save_with_base(&with_takes(&base, vec![edited]), &loaded.base_hashes);
    assert!(saved.is_ok());
    assert_eq!(f.live_count("tracks"), 0, "the accepted delete must stand");
    assert_eq!(
        f.live_count("practice_notes"),
        0,
        "a live note under a tombstoned track is an orphan"
    );
    assert_eq!(f.live_count("clips"), 0);
}

// --- a take whose interpretation resolves to none ---
//
// Sync leaves such takes: the integrity repair NULLs a pointer at an
// interpretation another device deleted, and one whose newer state is parked
// in the quarantine is not here yet. The take is LISTED under the active
// interpretation, but loading rewrites nothing: a load-time rewrite was an
// edit nobody made, whose next save — any save — outranked other devices'
// concurrent changes and the parked interpretation's own.

mod orphaned_take {
    use super::*;

    fn repair(f: &Fixture) {
        let writer = Arc::new(SyncWriter::new(
            f.db.clone(),
            Arc::new(HlcClock::new(pad_id("FIX"))),
        ));
        let checker = IntegrityChecker::new(f.db.clone(), writer);
        f.db.transaction(|| checker.repair()).unwrap();
    }

    struct Orphaned {
        pr: SongPractice,
        orphan: AudioTrack,
        other: AudioTrack,
    }

    /// Two interpretations; the ACTIVE one is deleted by another device and
    /// the repair NULLs both pointers at it.
    fn deleted_under_it(f: &Fixture) -> Orphaned {
        let keep = TakeVersion::new(new_id(), "Keep", d2026());
        let gone = TakeVersion::new(new_id(), "Gone", d2026());
        let orphan = take("Take 1", Some(&gone.id));
        let other = take("Take 2", Some(&keep.id));
        let pr = f.save(&SongPractice {
            versions: arcs(vec![keep, gone.clone()]),
            active_version_id: Some(gone.id.clone()),
            vocal_tracks: arcs(vec![orphan.clone(), other.clone()]),
            ..SongPractice::new(new_id(), "Song", d2026(), d2026())
        });
        f.raw(
            "UPDATE interpretations SET deleted_at = 1 WHERE id = ?",
            [&gone.id],
        );
        repair(f);
        assert_eq!(f.track_row(&orphan.id).interpretation_id, None);
        Orphaned { pr, orphan, other }
    }

    /// Saves [loaded] with only the OTHER take's volume changed.
    fn unrelated_save(f: &Fixture, loaded: &Loaded<SongPractice>, other_id: &str) {
        let tracks = loaded
            .aggregate
            .vocal_tracks
            .iter()
            .map(|t| {
                if t.id == other_id {
                    AudioTrack {
                        volume: 0.3,
                        ..(**t).clone()
                    }
                } else {
                    (**t).clone()
                }
            })
            .collect();
        f.save_with_base(&with_takes(&loaded.aggregate, tracks), &loaded.base_hashes)
            .unwrap();
    }

    #[test]
    fn is_listed_under_the_active_interpretation_and_armable() {
        let f = Fixture::new();
        let o = deleted_under_it(&f);
        let got = f.load(&o.pr.id).aggregate;
        assert_eq!(
            got.active_vocal_tracks()
                .iter()
                .map(|t| t.id.clone())
                .collect::<Vec<_>>(),
            vec![o.orphan.id.clone(), o.other.id.clone()]
        );
        let orphan = got
            .vocal_tracks
            .iter()
            .find(|t| t.id == o.orphan.id)
            .unwrap();
        assert!(got.lists_take(orphan));
        assert_eq!(orphan.version_id, None, "loaded verbatim");
    }

    #[test]
    fn a_remote_delete_of_it_survives_an_unrelated_save() {
        let f = Fixture::new();
        let o = deleted_under_it(&f);
        let loaded = f.load(&o.pr.id);
        f.raw(
            "UPDATE tracks SET deleted_at = 5, hlc_packed = 'zzzz' WHERE id = ?",
            [&o.orphan.id],
        );
        unrelated_save(&f, &loaded, &o.other.id);
        assert!(f.track_row(&o.orphan.id).deleted_at.is_some());
    }

    #[test]
    fn remote_edits_of_it_and_of_the_practice_survive_an_unrelated_save() {
        let f = Fixture::new();
        let o = deleted_under_it(&f);
        let loaded = f.load(&o.pr.id);
        f.raw(
            "UPDATE tracks SET display_name = 'Remote name', hlc_packed = 'zzzz' WHERE id = ?",
            [&o.orphan.id],
        );
        f.raw(
            "UPDATE practices SET title = 'Remote title', hlc_packed = 'zzzz' WHERE id = ?",
            [&o.pr.id],
        );
        unrelated_save(&f, &loaded, &o.other.id);
        assert_eq!(f.track_row(&o.orphan.id).display_name, "Remote name");
        let practice: PracticeRow = f.db.find(&o.pr.id).unwrap().unwrap();
        assert_eq!(practice.title, "Remote title");
    }

    #[test]
    fn a_take_whose_interpretation_is_parked_stays_its_own_when_the_park_lands() {
        let f = Fixture::new();
        let keep = TakeVersion::new(new_id(), "Keep", d2026());
        let x = TakeVersion::new(new_id(), "X", d2026());
        let on_x = take("Take on X", Some(&x.id));
        let other = take("Take on Keep", Some(&keep.id));
        let pr = f.save(&SongPractice {
            versions: arcs(vec![keep.clone(), x.clone()]),
            active_version_id: Some(keep.id.clone()),
            vocal_tracks: arcs(vec![on_x.clone(), other.clone()]),
            ..SongPractice::new(new_id(), "Song", d2026(), d2026())
        });
        // X is tombstoned here, but a NEWER live state of it is parked (an
        // edit that beat the delete, held by the drift gate): the repair
        // defers.
        f.raw(
            "UPDATE interpretations SET deleted_at = 1 WHERE id = ?",
            [&x.id],
        );
        let held = Hlc::parse(&f.column::<String>("interpretations", "hlc_packed", &x.id)).unwrap();
        let parked = Hlc::new(held.wall_ms + 1000, 0, pad_id("PEER")).packed();
        QuarantineStore::new(f.db.clone())
            .put(&QuarantineEntry {
                table: "interpretations",
                id: &x.id,
                hlc_packed: &parked,
                schema_v: 1,
                payload: "{}",
                reason: "hlc-drift: parked by the test",
                quarantined_at: 0,
            })
            .unwrap();
        repair(&f);
        assert_eq!(
            f.track_row(&on_x.id).interpretation_id.as_deref(),
            Some(x.id.as_str())
        );

        let loaded = f.load(&pr.id);
        assert_eq!(
            loaded
                .aggregate
                .active_vocal_tracks()
                .iter()
                .map(|t| t.id.clone())
                .collect::<Vec<_>>(),
            vec![on_x.id.clone(), other.id.clone()],
            "listed meanwhile"
        );
        unrelated_save(&f, &loaded, &other.id);
        assert_eq!(
            f.track_row(&on_x.id).interpretation_id.as_deref(),
            Some(x.id.as_str())
        );
    }
}

// --- a backing lane coming or going ---
//
// A take's order_index counted the accompaniment and reference slots in
// front of it, so importing or removing a backing renumbered every take: each
// was rewritten under a fresh stamp as an edit nobody made, and a take
// another device had changed meanwhile was reverted to this editor's stale
// copy (both sides "changed", the active editor wins).

mod backing_lane {
    use super::*;

    struct ThreeTakes {
        pr: SongPractice,
        takes: Vec<AudioTrack>,
    }

    fn three_takes(f: &Fixture) -> ThreeTakes {
        let base = f.create("S");
        let takes: Vec<AudioTrack> = (1..=3)
            .map(|i| take(&format!("Take {i}"), Some(&base.versions[0].id)))
            .collect();
        let pr = f.save(&with_takes(&base, takes.clone()));
        ThreeTakes { pr, takes }
    }

    fn backing(kind: AudioTrackKind) -> Arc<AudioTrack> {
        Arc::new(lane(kind, kind.name()))
    }

    fn take_updates(f: &Fixture, takes: &[AudioTrack]) -> i64 {
        let ids = takes
            .iter()
            .map(|t| format!("'{}'", t.id))
            .collect::<Vec<_>>()
            .join(", ");
        f.count(
            &format!(
                "SELECT COUNT(*) FROM oplog WHERE entity_table = 'tracks' \
                 AND op = 'update' AND entity_id IN ({ids})"
            ),
            [],
        )
    }

    fn take_ids(p: &SongPractice) -> Vec<String> {
        p.vocal_tracks.iter().map(|t| t.id.clone()).collect()
    }

    #[test]
    fn leaves_every_take_row_as_it_was() {
        let f = Fixture::new();
        let t = three_takes(&f);
        let loaded = f.load(&t.pr.id);
        let added = f
            .save_with_base(
                &SongPractice {
                    accompaniment: Some(backing(AudioTrackKind::Accompaniment)),
                    reference: Some(backing(AudioTrackKind::Reference)),
                    ..loaded.aggregate.clone()
                },
                &loaded.base_hashes,
            )
            .unwrap();
        assert_eq!(take_updates(&f, &t.takes), 0, "backings added");
        f.save(&SongPractice {
            accompaniment: None,
            reference: None,
            ..added.aggregate.clone()
        });
        assert_eq!(take_updates(&f, &t.takes), 0, "backings removed");
        assert_eq!(
            take_ids(&f.load(&t.pr.id).aggregate),
            t.takes.iter().map(|t| t.id.clone()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn keeps_another_devices_edit_of_a_take_it_never_touched() {
        let f = Fixture::new();
        let t = three_takes(&f);
        let loaded = f.load(&t.pr.id);
        f.raw(
            "UPDATE tracks SET display_name = 'Remote name', hlc_packed = 'zzzz' WHERE id = ?",
            [&t.takes[1].id],
        );
        let saved = f.save_with_base(
            &SongPractice {
                reference: Some(backing(AudioTrackKind::Reference)),
                ..loaded.aggregate.clone()
            },
            &loaded.base_hashes,
        );
        assert!(saved.is_ok());
        assert_eq!(f.track_row(&t.takes[1].id).display_name, "Remote name");
    }

    #[test]
    fn a_practice_numbered_with_the_backings_in_front_keeps_its_take_order() {
        // What every earlier build wrote: accompaniment 0, reference 1, then
        // the takes. Their order here is deliberately not their id order.
        let f = Fixture::new();
        let t = three_takes(&f);
        f.save(&SongPractice {
            accompaniment: Some(backing(AudioTrackKind::Accompaniment)),
            reference: Some(backing(AudioTrackKind::Reference)),
            ..t.pr.clone()
        });
        let legacy = [&t.takes[2], &t.takes[0], &t.takes[1]];
        for (i, l) in legacy.iter().enumerate() {
            f.raw(
                "UPDATE tracks SET order_index = ? WHERE id = ?",
                rusqlite::params![i as i64 + 2, l.id],
            );
        }
        f.raw(
            "UPDATE tracks SET order_index = 1 WHERE kind = 'reference'",
            [],
        );
        let loaded = f.load(&t.pr.id);
        let legacy_ids: Vec<String> = legacy.iter().map(|t| t.id.clone()).collect();
        assert_eq!(take_ids(&loaded.aggregate), legacy_ids);
        let fourth = take("Take 4", Some(&t.pr.versions[0].id));
        let mut tracks: Vec<AudioTrack> = loaded
            .aggregate
            .vocal_tracks
            .iter()
            .map(|t| (**t).clone())
            .collect();
        tracks.push(fourth.clone());
        let saved = f.save_with_base(&with_takes(&loaded.aggregate, tracks), &loaded.base_hashes);
        assert!(saved.is_ok());
        let mut want = legacy_ids;
        want.push(fourth.id.clone());
        assert_eq!(take_ids(&f.load(&t.pr.id).aggregate), want);
    }
}

// --- a row whose stored JSON this build reads differently ---
//
// Hydration decodes the JSON columns into the model (clamping values out of
// range, dropping keys it doesn't know), but the base hash was the stored
// string's. Any save then saw such a row as edited: it rewrote it as a change
// nobody made, stripping what another build had written, and a concurrent
// edit of that row by another device was reverted.

mod json_read_differently {
    use super::*;

    fn encode(v: &serde_json::Value) -> String {
        dart_json::encode(v).unwrap()
    }

    #[test]
    fn the_practice_row_reverb_and_lyrics() {
        let f = Fixture::new();
        let base = f.create("S");
        let pr = f.save(&with_takes(
            &base,
            vec![take("Take 1", Some(&base.versions[0].id))],
        ));
        let mut reverb = ReverbSettings::DEFAULT.to_json();
        reverb["roomSize"] = json!(1.5);
        let reverb = encode(&reverb);
        let lyrics = encode(&json!([{"text": "la", "timeMs": 10, "style": "bold"}]));
        f.raw(
            "UPDATE practices SET reverb_json = ?, lyrics_json = ? WHERE id = ?",
            [reverb.as_str(), lyrics.as_str(), pr.id.as_str()],
        );
        let loaded = f.load(&pr.id);
        let before = f.ops_for("practices", &pr.id);
        f.raw(
            "UPDATE practices SET title = 'Remote title', hlc_packed = 'zzzz' WHERE id = ?",
            [&pr.id],
        );
        let panned = AudioTrack {
            pan: 0.5,
            ..(*loaded.aggregate.vocal_tracks[0]).clone()
        };
        let saved = f
            .save_with_base(
                &with_takes(&loaded.aggregate, vec![panned]),
                &loaded.base_hashes,
            )
            .unwrap();
        assert_eq!(
            f.column::<String>("practices", "title", &pr.id),
            "Remote title"
        );
        assert_eq!(
            f.column::<String>("practices", "reverb_json", &pr.id),
            reverb
        );
        assert_eq!(
            f.column::<String>("practices", "lyrics_json", &pr.id),
            lyrics
        );
        assert_eq!(f.ops_for("practices", &pr.id), before);

        // A real edit of the row is still written, as the model holds it.
        let edited = SongPractice {
            reverb: Some(ReverbSettings {
                wet: 0.5,
                ..saved.aggregate.reverb.unwrap()
            }),
            ..saved.aggregate.clone()
        };
        f.save_with_base(&edited, &saved.base_hashes).unwrap();
        assert_eq!(
            f.column::<String>("practices", "reverb_json", &pr.id),
            encode(&edited.reverb.unwrap().to_json())
        );
        assert_eq!(f.ops_for("practices", &pr.id), before + 1);
    }

    #[test]
    fn a_take_and_an_fx_bus_their_chains_and_sends() {
        let f = Fixture::new();
        let base = f.create("S");
        let bus = FxBus::new(new_id(), "Bus", TrackFx::flat());
        let lane_of = |name: &str| AudioTrack {
            fx: Some(TrackFx::flat()),
            sends: vec![TrackSend::new(bus.id.clone(), -9.0)],
            ..take(name, Some(&base.versions[0].id))
        };
        let held = lane_of("Take 1");
        let other = lane_of("Take 2");
        let pr = f.save(&SongPractice {
            vocal_tracks: arcs(vec![held.clone(), other.clone()]),
            fx_buses: arcs(vec![bus.clone()]),
            ..base.clone()
        });
        let mut fx = TrackFx::flat().to_json();
        fx["future"] = json!(1);
        let fx = encode(&fx);
        let mut send = TrackSend::new(bus.id.clone(), -9.0).to_json();
        send["future"] = json!(1);
        let sends = encode(&json!([send]));
        f.raw(
            "UPDATE tracks SET fx_json = ?, sends_json = ? WHERE id = ?",
            [fx.as_str(), sends.as_str(), held.id.as_str()],
        );
        f.raw(
            "UPDATE fx_buses SET fx_json = ? WHERE id = ?",
            [fx.as_str(), bus.id.as_str()],
        );
        let loaded = f.load(&pr.id);
        let held_ops = f.ops_for("tracks", &held.id);
        let bus_ops = f.ops_for("fx_buses", &bus.id);
        f.raw(
            "UPDATE tracks SET display_name = 'Remote name', hlc_packed = 'zzzz' WHERE id = ?",
            [&held.id],
        );
        let tracks = loaded
            .aggregate
            .vocal_tracks
            .iter()
            .map(|t| {
                if t.id == other.id {
                    AudioTrack {
                        volume: 0.3,
                        ..(**t).clone()
                    }
                } else {
                    (**t).clone()
                }
            })
            .collect();
        let saved = f.save_with_base(&with_takes(&loaded.aggregate, tracks), &loaded.base_hashes);
        assert!(saved.is_ok());
        assert_eq!(
            f.column::<String>("tracks", "display_name", &held.id),
            "Remote name"
        );
        assert_eq!(f.column::<String>("tracks", "fx_json", &held.id), fx);
        assert_eq!(f.column::<String>("tracks", "sends_json", &held.id), sends);
        assert_eq!(f.column::<String>("fx_buses", "fx_json", &bus.id), fx);
        assert_eq!(f.ops_for("tracks", &held.id), held_ops);
        assert_eq!(f.ops_for("fx_buses", &bus.id), bus_ops);
    }
}
