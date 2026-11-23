//! Port of `client/test/technique_repository_drift_test.dart`.

use std::fs;
use std::path::Path;

use indexmap::IndexMap;
use vocara_model::models::technique::Technique;
use vocara_model::models::technique_media::{TechniqueMedia, TechniqueMediaKind};
use vocara_model::util::dart_date_time::DateTime;
use vocara_model::util::dart_path as p;
use vocara_model::util::id::new_id;
use vocara_store::db::rows::{TechniqueMediaRow, TechniqueRow};
use vocara_store::repositories::TechniqueRepository;
use vocara_store::sync::hlc::Hlc;

use super::{d2026, pad_id, FailNextLoad, Fixture, Statements};

/// An audio example over a fresh file `root/<name>` holding [bytes],
/// labelled with its name.
fn media(f: &Fixture, name: &str, bytes: &[u8]) -> TechniqueMedia {
    TechniqueMedia {
        id: new_id(),
        path: f.file(name, bytes),
        kind: TechniqueMediaKind::Audio,
        label: name.to_string(),
        added_at: d2026(),
    }
}

/// `Technique(id:, name:, tags: ['EVT'], summary: 'bright edge', media:)`.
fn technique(name: &str, media: Vec<TechniqueMedia>) -> Technique {
    Technique {
        tags: vec!["EVT".to_string()],
        summary: "bright edge".to_string(),
        media,
        ..Technique::new(new_id(), name, d2026(), d2026())
    }
}

#[test]
fn an_edit_or_delete_over_a_row_stamped_ahead_of_this_clock_orders_after_it() {
    // See the practice repository's twin: a second running instance's stamps
    // can sit ahead of this clock.
    let f = Fixture::new();
    let m = media(&f, "ex.wav", &[1, 2, 3]);
    let t = f.save_technique(&technique("Twang", vec![m.clone()]));
    let ahead = Hlc::new(
        DateTime::now().millis_since_epoch() + 60000,
        7,
        pad_id("OTHER"),
    )
    .packed();
    let plant = |table: &str, id: &str| {
        f.raw(
            &format!("UPDATE {table} SET hlc_packed = ? WHERE id = ?"),
            [ahead.as_str(), id],
        )
    };
    let held = |table: &str, id: &str| f.column::<String>(table, "hlc_packed", id);

    plant("techniques", &t.id);
    f.techs
        .save_technique(&Technique {
            name: "Belt".into(),
            ..t.clone()
        })
        .unwrap();
    assert!(held("techniques", &t.id) > ahead);

    plant("techniques", &t.id);
    plant("technique_media", &m.id);
    f.techs.delete_technique(&t.id).unwrap();
    assert!(held("techniques", &t.id) > ahead);
    assert!(held("technique_media", &m.id) > ahead);
}

#[test]
fn save_then_load_round_trips_a_technique_and_resolves_its_media_to_a_path() {
    let f = Fixture::new();
    let m = media(&f, "ex.wav", &[1, 2, 3]);
    let t = technique("Twang", vec![m.clone()]);
    let saved = f.save_technique(&t);

    assert_eq!(saved.id, t.id);
    assert_eq!(saved.name, "Twang");
    assert_eq!(saved.tags, vec!["EVT".to_string()]);
    assert_eq!(saved.media.len(), 1);
    // The returned path is the pooled location, and it's readable.
    let pool = p::join(&f.root().to_string_lossy(), "assets");
    assert!(p::is_within(&pool, &saved.media[0].path));
    assert_eq!(fs::read(&saved.media[0].path).unwrap(), vec![1, 2, 3]);

    let got = f.techs.get_technique(&t.id).unwrap();
    assert_eq!(got.name, "Twang");
    assert_eq!(got.media[0].id, m.id);
}

#[test]
fn editing_the_name_bumps_updated_at_and_an_unchanged_save_is_a_no_op() {
    let f = Fixture::new();
    let v1 = f.save_technique(&technique("Twang", Vec::new()));

    // No-op save: nothing changed -> updatedAt stays, no extra oplog rows.
    let before_ops = f.oplog().len();
    let v1b = f.save_technique(&v1);
    assert_eq!(v1b.updated_at, v1.updated_at);
    assert_eq!(f.oplog().len(), before_ops);

    // Real edit -> updatedAt advances + one more oplog row.
    let v2 = f.save_technique(&Technique {
        name: "Belting".into(),
        ..v1.clone()
    });
    assert_eq!(v2.name, "Belting");
    assert!(v2.updated_at.is_after(&v1.updated_at));
    assert_eq!(f.oplog().len(), before_ops + 1);
}

#[test]
fn media_is_added_and_removed_via_reconcile_and_identical_bytes_dedup() {
    let f = Fixture::new();
    let m1 = media(&f, "a.wav", &[1, 1]);
    let mut t = f.save_technique(&technique("Twang", vec![m1]));

    // Add a second example.
    let m2 = media(&f, "b.wav", &[2, 2]);
    let mut more = t.media.clone();
    more.push(m2);
    t = f.save_technique(&Technique {
        media: more,
        ..t.clone()
    });
    assert_eq!(t.media.len(), 2);
    assert_eq!(f.count("SELECT COUNT(*) FROM assets", []), 2);

    // Remove the first -> domain shows one; the row is tombstoned, asset
    // kept.
    t = f.save_technique(&Technique {
        media: vec![t.media[1].clone()],
        ..t.clone()
    });
    assert_eq!(t.media.len(), 1);
    assert_eq!(f.live_count("technique_media"), 1);
}

#[test]
fn every_technique_field_round_trips_no_column_silently_dropped() {
    // The counterpart of the practice repository's field-by-field
    // round-trip: every user-editable field on the technique and on each
    // media row set to a distinct non-default value, so a column the writer
    // forgets — or the hydrator never reads — surfaces here instead of as a
    // lost edit.
    let f = Fixture::new();
    let a = media(&f, "a.wav", &[1, 2, 3]);
    let b = media(&f, "b.wav", &[4, 5, 6]);
    let t = Technique {
        aliases: vec!["Chest mix".into(), "Yell".into()],
        tags: vec!["CVT".into(), "EVT".into(), "loud".into()],
        summary: "a summary".into(),
        description: "a description".into(),
        cues: "some cues".into(),
        exercises: "some exercises".into(),
        acoustic_signature: "high SPL, strong 2-4 kHz".into(),
        media: vec![
            TechniqueMedia {
                label: "first label".into(),
                ..a
            },
            TechniqueMedia {
                kind: TechniqueMediaKind::Video,
                label: "second label".into(),
                ..b
            },
        ],
        ..Technique::new(
            new_id(),
            "Belt",
            DateTime::local_ymd(2026, 1, 2),
            DateTime::local_ymd(2026, 3, 4),
        )
    };
    f.techs.save_technique(&t).unwrap();

    let l = f.techs.get_technique(&t.id).unwrap();
    assert_eq!(l.name, "Belt");
    assert_eq!(l.aliases, vec!["Chest mix", "Yell"]);
    assert_eq!(l.tags, vec!["CVT", "EVT", "loud"]);
    assert_eq!(l.summary, "a summary");
    assert_eq!(l.description, "a description");
    assert_eq!(l.cues, "some cues");
    assert_eq!(l.exercises, "some exercises");
    assert_eq!(l.acoustic_signature, "high SPL, strong 2-4 kHz");
    assert_eq!(
        l.media.iter().map(|m| m.label.as_str()).collect::<Vec<_>>(),
        vec!["first label", "second label"]
    );
    assert_eq!(
        l.media.iter().map(|m| m.kind).collect::<Vec<_>>(),
        vec![TechniqueMediaKind::Audio, TechniqueMediaKind::Video]
    );
    // The order the user arranged them in is part of the content.
    assert_eq!(
        l.media.iter().map(|m| m.id.clone()).collect::<Vec<_>>(),
        vec![t.media[0].id.clone(), t.media[1].id.clone()]
    );
    assert_eq!(fs::read(&l.media[0].path).unwrap(), vec![1, 2, 3]);
    assert_eq!(fs::read(&l.media[1].path).unwrap(), vec![4, 5, 6]);
}

#[test]
fn list_and_delete_tombstone() {
    let f = Fixture::new();
    let a = f.save_technique(&technique("A", Vec::new()));
    f.save_technique(&technique("B", Vec::new()));
    assert_eq!(f.techs.list_techniques().unwrap().len(), 2);

    f.techs.delete_technique(&a.id).unwrap();
    assert_eq!(f.techs.list_techniques().unwrap().len(), 1);
    assert!(f.techs.get_technique(&a.id).is_err());
}

#[test]
fn a_technique_whose_tags_are_not_text_is_left_out_of_the_list() {
    // Not handed to the library to fail on. The hydrator cast the decoded
    // tags lazily: the row built fine, the list came back Ok with it, and the
    // library threw a TypeError on its first pass over the tags, taking the
    // whole screen down.
    let f = Fixture::new();
    f.save_technique(&technique("Good", Vec::new()));
    let bad = f.save_technique(&technique("Bad", Vec::new()));
    f.raw(
        "UPDATE techniques SET tags_json = ? WHERE id = ?",
        [r#"[{"name": "CVT"}]"#, bad.id.as_str()],
    );
    let listed = f.techs.list_techniques().unwrap();
    assert_eq!(
        listed.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
        vec!["Good"]
    );
}

#[test]
fn the_library_lists_in_the_same_few_reads_however_many_techniques_and_examples_it_holds() {
    // Each example in order with its path.
    let reads = Statements::default();
    let f = Fixture::with_db(reads.database());
    let mut made = 0u8;
    let mut list_cost = |add: u8| -> usize {
        for _ in 0..add {
            let name = format!("T{made}");
            let examples = vec![
                media(&f, &format!("a{made}.wav"), &[made, 1]),
                media(&f, &format!("b{made}.wav"), &[made, 2]),
            ];
            f.save_technique(&technique(&name, examples));
            made += 1;
        }
        reads.clear();
        let listed = f.techs.list_techniques().unwrap();
        let cost = reads.count();
        assert_eq!(listed.len(), usize::from(made));
        for t in &listed {
            let n = &t.name[1..];
            assert_eq!(
                t.media.iter().map(|m| m.label.clone()).collect::<Vec<_>>(),
                vec![format!("a{n}.wav"), format!("b{n}.wav")]
            );
            for m in &t.media {
                assert!(Path::new(&m.path).exists());
            }
        }
        cost
    };

    // techniques + their media + those media's assets. It was 1 + one per
    // technique + one per example (7 vs 31 here).
    let few = list_cost(2);
    assert_eq!(list_cost(8), few);
    assert_eq!(few, 3);
}

#[test]
fn a_row_whose_stored_json_or_kind_this_build_reads_differently_is_no_edit() {
    // An untouched save leaves it, and keeps a concurrent rename. Hydration
    // decodes the alias/tag JSON and folds an example kind it does not know
    // into audio, but the base hash was the stored row's: any save rewrote
    // such rows as edits nobody made, reverting a concurrent edit.
    let f = Fixture::new();
    let saved = f.save_technique(&technique("Twang", vec![media(&f, "a.wav", &[1])]));
    let media_id = saved.media[0].id.clone();
    f.raw(
        "UPDATE techniques SET aliases_json = ?, tags_json = ? WHERE id = ?",
        [r#"["belt", "call"]"#, r#"[ "EVT" ]"#, saved.id.as_str()],
    );
    f.raw(
        "UPDATE technique_media SET kind = 'image' WHERE id = ?",
        [&media_id],
    );
    let loaded = f.techs.load_technique(&saved.id).unwrap();
    let before = f.oplog().len();
    f.raw(
        "UPDATE techniques SET name = 'Remote', hlc_packed = 'zzzz' WHERE id = ?",
        [&saved.id],
    );
    f.techs
        .save_technique_with_base(&loaded.aggregate, &loaded.base_hashes)
        .unwrap();
    let row: TechniqueRow = f.db.find(&saved.id).unwrap().unwrap();
    assert_eq!(row.name, "Remote");
    assert_eq!(row.aliases_json.as_deref(), Some(r#"["belt", "call"]"#));
    assert_eq!(row.tags_json.as_deref(), Some(r#"[ "EVT" ]"#));
    let m: TechniqueMediaRow = f.db.find(&media_id).unwrap().unwrap();
    assert_eq!(m.kind, "image");
    assert_eq!(f.oplog().len(), before);
}

// Reported as failed, the editor reverted to the technique before the
// committed rename and kept the base before it; its next save took the
// stored rename for a remote edit and wrote the old name back.
#[test]
fn a_save_whose_reread_fails_still_reports_what_it_committed() {
    let f = Fixture::new();
    let flaky = f.other_techs();
    let fail = FailNextLoad::default();
    flaky.set_debug_load_hook(Some(fail.hook()));
    let t = Technique::new(new_id(), "Belt", d2026(), d2026());
    let first = flaky
        .save_technique_with_base(&t, &IndexMap::new())
        .unwrap();
    fail.arm();
    let renamed = Technique {
        name: "Belt 2".into(),
        ..first.aggregate.clone()
    };
    let saved = flaky
        .save_technique_with_base(&renamed, &first.base_hashes)
        .unwrap();
    assert!(!saved.reread);
    assert_eq!(saved.aggregate.name, "Belt 2");

    // The next save merges against what was sent: an unrelated edit keeps
    // the rename.
    flaky
        .save_technique_with_base(
            &Technique {
                summary: "Bright, forward".into(),
                ..saved.aggregate.clone()
            },
            &saved.base_hashes,
        )
        .unwrap();
    let row = f.db.select_all::<TechniqueRow>().unwrap().remove(0);
    assert_eq!(row.name, "Belt 2");
    assert_eq!(row.summary, "Bright, forward");
}
