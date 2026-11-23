//! Port of the repository-level cases of
//! `client/test/integrity_checker_test.dart`: the checker over what the
//! song repository writes.
//!
//! The Dart suite seeded every case through the repository; the checker's
//! own tests (`sync::integrity_checker`) seed the same rows by hand. These
//! keep the cases whose input is the repository's output — a practice, a
//! take and its clip, FX buses — so a change in what a save writes that the
//! checker would misread surfaces here.

use std::sync::Arc;

use vocara_model::models::audio_fx::TrackFx;
use vocara_model::models::audio_track::{AudioClip, AudioTrack};
use vocara_model::models::fx_bus::FxBus;
use vocara_model::models::song_practice::SongPractice;
use vocara_model::util::id::new_id;
use vocara_store::sync::hlc::Hlc;
use vocara_store::sync::integrity_checker::IntegrityChecker;

use super::{arcs, pad_id, take, with_takes, Fixture};

fn checker(f: &Fixture) -> IntegrityChecker {
    IntegrityChecker::new(f.db.clone(), f.writer.clone())
}

/// Seeds a practice with one vocal take (one clip) and returns it.
fn seed(f: &Fixture) -> SongPractice {
    let base = f.create("S");
    let vocal = AudioTrack {
        clips: vec![AudioClip::new(
            new_id(),
            f.file("take.wav", &[1, 2, 3]),
            1000,
        )],
        ..take("Take", Some(&base.versions[0].id))
    };
    f.save(&with_takes(&base, vec![vocal]))
}

fn raw_tombstone(f: &Fixture, table: &str, id: &str) {
    f.raw(
        &format!("UPDATE {table} SET deleted_at = 999 WHERE id = ?"),
        [id],
    );
}

fn first_id(f: &Fixture, table: &str) -> String {
    f.db.query_row(
        &format!("SELECT id FROM {table} WHERE deleted_at IS NULL LIMIT 1"),
        [],
        |r| r.get(0),
    )
    .unwrap()
}

fn invariants(c: &IntegrityChecker) -> Vec<String> {
    c.check()
        .unwrap()
        .into_iter()
        .map(|v| v.invariant)
        .collect()
}

#[test]
fn a_well_formed_practice_has_no_violations() {
    let f = Fixture::new();
    seed(&f);
    assert!(checker(&f).check().unwrap().is_empty());
}

#[test]
fn a_live_clip_under_a_tombstoned_track_is_detected_and_cascade_repaired() {
    let f = Fixture::new();
    seed(&f);
    raw_tombstone(&f, "tracks", &first_id(&f, "tracks"));

    let c = checker(&f);
    assert!(invariants(&c).contains(&"clip-track".to_string()));

    assert!(c.repair().unwrap().is_empty());
    // The orphaned clip is now tombstoned too.
    assert_eq!(f.live_count("clips"), 0);
}

#[test]
fn a_dangling_active_interpretation_id_is_nulled() {
    let f = Fixture::new();
    let pr = seed(&f);
    raw_tombstone(&f, "interpretations", &pr.versions[0].id);

    let c = checker(&f);
    assert!(invariants(&c).contains(&"active-interp".to_string()));

    c.repair().unwrap();
    assert_eq!(
        f.column::<Option<String>>("practices", "active_interpretation_id", &pr.id),
        None
    );
    assert!(c.check().unwrap().is_empty());
}

#[test]
fn a_bus_route_is_nulled_when_its_target_is_tombstoned_or_foreign_and_left_while_absent() {
    let f = Fixture::new();
    let base = seed(&f);
    let a = FxBus::new(new_id(), "A", TrackFx::flat());
    let b = FxBus::new(new_id(), "B", TrackFx::flat());
    f.save(&SongPractice {
        fx_buses: arcs(vec![a.clone(), b.clone()]),
        ..base.clone()
    });
    let route_of_a = || f.column::<Option<String>>("fx_buses", "output_bus_id", &a.id);
    let route_a = |to: &str| {
        f.raw(
            "UPDATE fx_buses SET output_bus_id = ? WHERE id = ?",
            [to, a.id.as_str()],
        )
    };
    let c = checker(&f);

    // Absent: pending.
    let absent = new_id();
    route_a(&absent);
    assert!(c.check().unwrap().is_empty());
    c.repair().unwrap();
    assert_eq!(route_of_a(), Some(absent));

    // Tombstoned: dangling.
    route_a(&b.id);
    raw_tombstone(&f, "fx_buses", &b.id);
    assert!(invariants(&c).contains(&"bus-output".to_string()));
    assert!(c.repair().unwrap().is_empty());
    assert_eq!(route_of_a(), None);

    // Live, but another practice's bus: dangling.
    let other = f.create("Other");
    let foreign = FxBus::new(new_id(), "F", TrackFx::flat());
    f.save(&SongPractice {
        fx_buses: vec![Arc::new(foreign.clone())],
        ..other
    });
    route_a(&foreign.id);
    assert!(invariants(&c).contains(&"bus-output".to_string()));
    assert!(c.repair().unwrap().is_empty());
    assert_eq!(route_of_a(), None);
}

#[test]
fn a_bus_routing_cycle_synthesised_by_a_merge_is_broken_at_its_oldest_edge() {
    // Neither edit is illegal on its own — every device refuses to create a
    // cycle locally — but A→B applied offline on one device and B→A on
    // another merge into a ring. The raw UPDATEs stand in for that merge.
    let f = Fixture::new();
    let base = seed(&f);
    let a = FxBus::new(new_id(), "A", TrackFx::flat());
    let b = FxBus::new(new_id(), "B", TrackFx::flat());
    f.save(&SongPractice {
        fx_buses: arcs(vec![a.clone(), b.clone()]),
        ..base
    });

    // A is the older edit (lower HLC), so it is the one that must lose its
    // route: the newer B→A survives, matching the LWW rule everywhere else.
    let stamp = |wall: i64| Hlc::new(wall, 0, pad_id("PEER")).packed();
    f.raw(
        "UPDATE fx_buses SET output_bus_id = ?, hlc_packed = ? WHERE id = ?",
        [b.id.as_str(), &stamp(1000), a.id.as_str()],
    );
    f.raw(
        "UPDATE fx_buses SET output_bus_id = ?, hlc_packed = ? WHERE id = ?",
        [a.id.as_str(), &stamp(2000), b.id.as_str()],
    );

    let c = checker(&f);
    let found = c.check().unwrap();
    assert!(found.iter().all(|v| v.invariant == "bus-cycle"));
    let mut ids: Vec<String> = found.into_iter().map(|v| v.id).collect();
    ids.sort();
    ids.dedup();
    let mut want = vec![a.id.clone(), b.id.clone()];
    want.sort();
    assert_eq!(ids, want);

    assert!(c.repair().unwrap().is_empty());
    assert_eq!(
        f.column::<Option<String>>("fx_buses", "output_bus_id", &a.id),
        None,
        "the older edge is dropped"
    );
    assert_eq!(
        f.column::<Option<String>>("fx_buses", "output_bus_id", &b.id),
        Some(a.id.clone()),
        "the newer edge survives"
    );
}
