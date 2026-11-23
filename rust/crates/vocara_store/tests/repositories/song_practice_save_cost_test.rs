//! Port of `client/test/song_practice_save_cost_test.dart`.
//!
//! What a save and a load COST, pinned in statements: every statement is an
//! isolate round trip in the Flutter app (and a lock round trip here), so a
//! per-clip query inside a save or load turns a mute click on a 100-clip
//! practice into hundreds of hops. These tests hold the shape — set reads,
//! one batch — and that the batched paths leave the store exactly as the
//! per-clip paths did.
//!
//! The statements are SQLite's own trace of every statement the connection
//! ran (drift's interceptor saw drift's statements); the SQL text is this
//! port's, so the fragments matched are the Rust store's spelling of the
//! same queries.

use std::collections::HashMap;

use vocara_model::models::audio_track::{AudioClip, AudioTrack, AudioTrackKind};
use vocara_model::models::practice_note::PracticeNote;
use vocara_model::models::song_practice::SongPractice;
use vocara_model::models::take_version::TakeVersion;
use vocara_model::models::technique_marker::TechniqueMarker;
use vocara_model::util::dart_date_time::DateTime;
use vocara_model::util::id::new_id;
use vocara_store::db::rows::WaveformCacheRow;

use super::{arcs, Fixture, Statements};

struct Cost {
    f: Fixture,
    statements: Statements,
    file_seq: std::cell::Cell<usize>,
}

fn at1000() -> DateTime {
    DateTime::from_millis_since_epoch(1000, false).unwrap()
}

impl Cost {
    fn new() -> Self {
        let statements = Statements::default();
        Cost {
            f: Fixture::with_db(statements.database()),
            statements,
            file_seq: std::cell::Cell::new(0),
        }
    }

    fn clip(&self, bytes: &[u8], peaks: &[f64]) -> AudioClip {
        let seq = self.file_seq.get();
        self.file_seq.set(seq + 1);
        AudioClip {
            waveform_peaks: peaks.to_vec(),
            ..AudioClip::new(new_id(), self.f.file(&format!("src{seq}.wav"), bytes), 1000)
        }
    }

    /// A practice with [lanes] vocal takes of [clips_per_lane] distinct
    /// clips, one note and one marker each.
    fn practice(&self, lanes: usize, clips_per_lane: usize) -> SongPractice {
        let now = at1000();
        let v = TakeVersion::new(new_id(), "I1", now);
        let mut seed = 0u8;
        let tracks = (0..lanes)
            .map(|l| AudioTrack {
                version_id: Some(v.id.clone()),
                clips: (0..clips_per_lane)
                    .map(|c| {
                        seed += 1;
                        self.clip(
                            &[seed - 1, l as u8, c as u8],
                            &[0.25, 0.5, (c as f64 + 1.0) / 8.0],
                        )
                    })
                    .collect(),
                notes: vec![PracticeNote {
                    id: new_id(),
                    position_ms: 10,
                    end_ms: None,
                    content: format!("n{l}"),
                    created_at: now,
                    updated_at: now,
                }],
                technique_markers: vec![TechniqueMarker {
                    id: new_id(),
                    position_ms: 20,
                    end_ms: None,
                    technique_id: new_id(),
                    technique_name: "Twang".into(),
                    created_at: now,
                    updated_at: now,
                }],
                ..AudioTrack::new(new_id(), AudioTrackKind::Vocal, format!("Take {l}"), now)
            })
            .collect();
        SongPractice {
            versions: arcs(vec![v.clone()]),
            active_version_id: Some(v.id.clone()),
            vocal_tracks: arcs(tracks),
            ..SongPractice::new(new_id(), "Song", now, now)
        }
    }

    fn total_changes(&self) -> i64 {
        self.f.count("SELECT total_changes()", [])
    }
}

#[test]
fn a_save_resolves_every_pool_path_in_one_owner_read_and_an_unchanged_save_writes_no_row() {
    // Not even an identical waveform.
    let c = Cost::new();
    let pr = c.practice(3, 4);
    c.f.save(&pr);
    let loaded = c.f.load(&pr.id);

    c.statements.clear();
    let before = c.total_changes();
    c.f.save_with_base(&loaded.aggregate, &loaded.base_hashes)
        .unwrap();

    // 12 clips, one owner read (it was one per clip).
    assert_eq!(c.statements.matching("local_rel_path IN"), 1);
    assert_eq!(c.statements.matching("local_rel_path = ?"), 0);
    // Nothing changed, so nothing was written: no reconcile write, no
    // updatedAt touch, and no waveform blob rewritten over identical bytes
    // (the per-clip upserts rewrote all 12).
    assert_eq!(c.total_changes(), before);
}

#[test]
fn a_save_reads_each_child_table_once_however_many_lanes() {
    // It read every lane's clips, notes and markers with a query each: 90
    // statements on a 30-lane practice, inside the save's transaction.
    let c = Cost::new();
    let child_reads = |lanes: usize| -> usize {
        let pr = c.practice(lanes, 2);
        c.f.save(&pr);
        let loaded = c.f.load(&pr.id);
        // An edit on every lane, so every lane's children are reconciled.
        let edited = SongPractice {
            vocal_tracks: loaded
                .aggregate
                .vocal_tracks
                .iter()
                .map(|t| {
                    std::sync::Arc::new(AudioTrack {
                        notes: t
                            .notes
                            .iter()
                            .map(|n| PracticeNote {
                                content: "edited".into(),
                                ..n.clone()
                            })
                            .collect(),
                        ..(**t).clone()
                    })
                })
                .collect(),
            ..loaded.aggregate.clone()
        };
        c.statements.clear();
        c.f.save_with_base(&edited, &loaded.base_hashes).unwrap();
        // The save's own reads, not its re-read (hydration reads with IN
        // too).
        let all = c.statements.all();
        let per_track: Vec<usize> = ["clips", "practice_notes", "technique_markers"]
            .iter()
            .map(|table| {
                all.iter()
                    .filter(|q| {
                        q.starts_with("SELECT")
                            && q.contains(&format!("FROM \"{table}\""))
                            && q.contains("track_id = ?")
                    })
                    .count()
            })
            .collect();
        assert_eq!(per_track, vec![0, 0, 0], "{lanes} lanes");
        // drift reported a batch's statement once, whatever its argument
        // sets: the waveform cache's upserts (one per clip, in one batch)
        // count once here too.
        let batched = all
            .iter()
            .filter(|q| q.starts_with("INSERT INTO waveform_cache"))
            .count();
        all.len() - batched.saturating_sub(1)
    };

    let small = child_reads(2);
    let large = child_reads(12);
    // Writes still scale with the edits (one note update per lane); reads
    // must not add three per lane on top.
    assert!(large - small <= (12 - 2) * 4, "{small} vs {large}");
}

#[test]
fn the_waveform_cache_ends_as_the_per_clip_writes_left_it() {
    // Last write per content hash wins, in clip order.
    let c = Cost::new();
    let now = at1000();
    // Two clips over IDENTICAL bytes (one asset, one cache row) with
    // different peaks, then a third with its own bytes.
    let first = c.clip(&[7, 7, 7], &[0.25]);
    let second = c.clip(&[7, 7, 7], &[0.75]);
    let third = c.clip(&[8, 8], &[0.5, 0.125]);
    let pr = SongPractice {
        vocal_tracks: arcs(vec![AudioTrack {
            clips: vec![first, second, third],
            ..AudioTrack::new(new_id(), AudioTrackKind::Vocal, "Take", now)
        }]),
        ..SongPractice::new(new_id(), "Song", now, now)
    };
    c.f.save(&pr);

    let cache = || -> HashMap<usize, Vec<f64>> {
        c.f.db
            .select_all::<WaveformCacheRow>()
            .unwrap()
            .into_iter()
            .map(|r| {
                let floats = r
                    .peaks
                    .chunks_exact(4)
                    .map(|b| f64::from(f32::from_le_bytes([b[0], b[1], b[2], b[3]])))
                    .collect();
                (r.peaks.len(), floats)
            })
            .collect()
    };

    assert_eq!(c.f.db.select_all::<WaveformCacheRow>().unwrap().len(), 2);
    assert_eq!(cache()[&4], vec![0.75]); // the later clip's write
    assert_eq!(cache()[&8], vec![0.5, 0.125]);

    // New peaks for an existing hash replace the cached bytes.
    let loaded = c.f.load(&pr.id);
    let take = &loaded.aggregate.vocal_tracks[0];
    let edited = SongPractice {
        vocal_tracks: arcs(vec![AudioTrack {
            clips: take
                .clips
                .iter()
                .map(|clip| {
                    if clip.waveform_peaks.len() == 2 {
                        AudioClip {
                            waveform_peaks: vec![0.0625, 1.0],
                            ..clip.clone()
                        }
                    } else {
                        clip.clone()
                    }
                })
                .collect(),
            ..(**take).clone()
        }]),
        ..loaded.aggregate.clone()
    };
    c.f.save_with_base(&edited, &loaded.base_hashes).unwrap();
    assert_eq!(cache()[&8], vec![0.0625, 1.0]);
}

#[test]
fn a_load_costs_the_same_few_statements_whatever_the_practice_holds() {
    let c = Cost::new();
    let load_cost = |pr: &SongPractice| -> usize {
        c.f.save(pr);
        c.statements.clear();
        c.f.load(&pr.id);
        c.statements.count()
    };

    let small = load_cost(&c.practice(2, 1));
    let large = load_cost(&c.practice(9, 5));
    // practice + interpretations + tracks + buses, and one read each for
    // clips / notes / markers / their assets / their cached peaks. It was 3
    // per lane plus 3 per clip (16 vs 166 here).
    assert_eq!(large, small);
    assert!(large <= 9, "{large}");
}

#[test]
fn hydration_keeps_each_lanes_children_in_the_order_their_per_lane_reads_returned_them() {
    let c = Cost::new();
    let now = at1000();
    // Ids minted in REVERSE of list order, so any sort by id would visibly
    // reorder them: lanes come back by order index, notes and markers in
    // insertion (rowid) order.
    let mut ids: Vec<String> = (0..24).map(|_| new_id()).collect();
    ids.reverse();
    let mut ids = ids.into_iter();
    let mut next_id = move || ids.next().unwrap();
    let mut lanes = Vec::new();
    for name in ["one", "two", "three"] {
        // Drawn in the Dart constructor's argument order: the lane's id, its
        // notes, then its markers.
        let id = next_id();
        let notes = ["c", "a", "b"]
            .iter()
            .map(|suffix| PracticeNote {
                id: next_id(),
                position_ms: 0,
                end_ms: None,
                content: format!("{name}-{suffix}"),
                created_at: now,
                updated_at: now,
            })
            .collect();
        let markers = ["z", "y"]
            .iter()
            .map(|suffix| TechniqueMarker {
                id: next_id(),
                position_ms: 0,
                end_ms: None,
                technique_id: new_id(),
                technique_name: format!("{name}-{suffix}"),
                created_at: now,
                updated_at: now,
            })
            .collect();
        lanes.push(AudioTrack {
            notes,
            technique_markers: markers,
            ..AudioTrack::new(id, AudioTrackKind::Vocal, name, now)
        });
    }
    let pr = SongPractice {
        vocal_tracks: arcs(lanes),
        ..SongPractice::new(new_id(), "Song", now, now)
    };
    c.f.save(&pr);

    let got = c.f.load(&pr.id).aggregate;
    for (i, t) in got.vocal_tracks.iter().enumerate() {
        let want = &pr.vocal_tracks[i];
        assert_eq!(t.id, want.id);
        assert_eq!(
            t.notes
                .iter()
                .map(|n| n.content.clone())
                .collect::<Vec<_>>(),
            want.notes
                .iter()
                .map(|n| n.content.clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            t.technique_markers
                .iter()
                .map(|m| m.technique_name.clone())
                .collect::<Vec<_>>(),
            want.technique_markers
                .iter()
                .map(|m| m.technique_name.clone())
                .collect::<Vec<_>>()
        );
    }
}
