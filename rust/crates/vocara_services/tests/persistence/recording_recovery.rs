//! Port of `client/test/recording_recovery_test.dart`: boot-time recovery of
//! hard-kill orphaned recordings.
//!
//! A force quit / crash / kill -9 mid-record leaves the streamed take WAV in
//! the audio staging tree (audio/<practiceId>/<id>.wav) with no rows
//! referencing it; the engine keeps that file's header valid by patching it
//! in place ~1x/s. `Persistence::recover_staged_recordings` must re-attach
//! such a file to its practice as a new vocal take (analysis, clip, save —
//! the normal finalize shape), be idempotent across re-runs (a crash during
//! recovery must not duplicate the take), and leave everything else to the
//! staging sweep.
//!
//! The audio file service's own cases of that file (the WAV probe's
//! contract, the placement names, the aligned-start write order) are ported
//! with it in `tests/audio_file_service.rs`.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use vocara_model::models::audio_track::{AudioClip, AudioTrack, AudioTrackKind, TAKE_COLORS};
use vocara_model::models::song_practice::SongPractice;
use vocara_model::models::take_version::TakeVersion;
use vocara_model::util::dart_date_time::DateTime;
use vocara_model::util::dart_path;
use vocara_model::util::id::new_id;
use vocara_services::audio_file_service::{write_aligned_start_to, AudioFileService, AudioFiles};
use vocara_services::local_storage_service::LocalStorageService;
use vocara_services::persistence::{Persistence, WavAnalyzer};
use vocara_services::waveform_service::WaveformResult;
use vocara_store::db::AssetStore;
use vocara_store::repositories::SongPracticeRepository;

use crate::support::*;

fn recover(p: &Persistence, analyze: &WavAnalyzer) -> usize {
    p.recover_staged_recordings(analyze).unwrap()
}

/// Simulates the hard-kill file shape: real PCM bytes exist PAST what the
/// last in-place header patch covers (the header sizes are left as-is).
fn with_unpatched_tail(mut wav: Vec<u8>, extra_bytes: usize) -> Vec<u8> {
    wav.extend(std::iter::repeat_n(7u8, extra_bytes));
    wav
}

/// Writes an orphaned recording into the staging tree exactly where
/// `AudioFileService::new_recording_path` would have placed it.
fn stage_orphan(root: &Path, practice_id: &str, bytes: &[u8], name: Option<&str>) -> PathBuf {
    let name = name.map_or_else(|| format!("{}.wav", new_id()), str::to_string);
    let f = root.join("audio").join(practice_id).join(name);
    write(&f, bytes);
    f
}

/// Audio files rooted at the persistence root, the way production shares
/// one application-support directory between the store and the file
/// services.
fn rooted_files(root: &Path) -> AudioFileService {
    AudioFileService::new(Arc::new(LocalStorageService::with_root(root)))
}

/// A vocal take of [name] in [version] over the file [path].
fn take_over(path: &str, name: &str, version: Option<&str>) -> AudioTrack {
    AudioTrack {
        version_id: version.map(str::to_string),
        ..AudioTrack::new(new_id(), AudioTrackKind::Vocal, name, DateTime::now()).with_audio(
            path,
            0,
            100,
            Vec::new(),
        )
    }
}

/// `practice.copyWith(vocalTracks: tracks)`.
fn with_takes(p: &SongPractice, tracks: Vec<AudioTrack>) -> SongPractice {
    SongPractice {
        vocal_tracks: tracks.into_iter().map(Arc::new).collect(),
        ..p.clone()
    }
}

/// A fresh empty take saved into [practice] against its base: the empty
/// lane REC made for a take. Returns it.
fn save_empty_take(m: &Persistence, practice: &SongPractice) -> AudioTrack {
    let loaded = m.song_repo().load_practice(&practice.id).unwrap();
    let empty = AudioTrack {
        version_id: loaded.aggregate.active_version().map(|v| v.id.clone()),
        ..AudioTrack::new(
            new_id(),
            AudioTrackKind::Vocal,
            "Take 1",
            DateTime::local_ymd(2026, 1, 1),
        )
    };
    m.song_repo()
        .save_practice_with_base(
            &with_takes(&loaded.aggregate, vec![empty.clone()]),
            &loaded.base_hashes,
        )
        .unwrap();
    empty
}

fn set(paths: &[&Path]) -> HashSet<String> {
    paths.iter().map(|p| text(p)).collect()
}

#[test]
fn an_orphaned_recording_is_recovered_as_a_new_vocal_take() {
    let m = Memory::new();
    let practice = create_practice(&m);
    let orphan = stage_orphan(
        &m.root,
        &practice.id,
        &with_unpatched_tail(mono_wav(4800), 120),
        None,
    );

    assert_eq!(recover(&m, &fake_analyze()), 1);

    let after = reload(&m, &practice.id);
    assert_eq!(after.vocal_tracks.len(), 1);
    let take = &after.vocal_tracks[0];
    assert_eq!(take.kind, AudioTrackKind::Vocal);
    assert_eq!(take.display_name, "Take 1 (recovered)");
    assert_eq!(take.version_id, after.active_version_id);
    let clip = take.primary_clip().unwrap();
    assert_eq!(clip.duration_ms, 500); // from the (injected) waveform analysis
    assert_eq!(clip.start_ms, 0); // the killed session's playhead is unknowable
    assert_eq!(clip.waveform_peaks.len(), 2);
    assert!((clip.waveform_peaks[0] - 0.1).abs() < 1e-6);
    assert!((clip.waveform_peaks[1] - 0.2).abs() < 1e-6);
    // The audio was imported into the content-addressed pool; the clip
    // resolves to a live pool file, not the staging path.
    assert!(dart_path::is_within(
        &text(&m.root.join("assets")),
        &clip.audio_file_path
    ));
    assert!(Path::new(&clip.audio_file_path).exists());
    // The staging copy was deleted after the rows committed.
    assert!(!orphan.exists());
}

#[test]
fn a_crashed_take_fills_the_empty_take_rec_made_for_it_where_it_was_recorded() {
    // Its name said neither, so the take came back as a new lane at the
    // timeline's start, and the empty "Take 1" REC had saved stayed behind.
    let m = Memory::new();
    let practice = create_practice(&m);
    let empty = save_empty_take(&m, &practice);
    let name = dart_path::basename(
        &rooted_files(&m.root)
            .new_recording_path(&practice.id, "wav", Some(&empty.id), Some(4200))
            .unwrap(),
    );
    stage_orphan(&m.root, &practice.id, &mono_wav(4800), Some(&name));
    // Only the clip is new: renamed, the lane's row was rewritten (a field
    // another build wrote stripped) and journaled at boot as a local edit,
    // overriding whatever another device did to the lane meanwhile.
    m.db()
        .execute(
            "UPDATE tracks SET fx_json = json_set(COALESCE(fx_json, '{}'), '$.future', 1) \
             WHERE id = ?",
            [&empty.id],
        )
        .unwrap();
    let lane_row = || {
        let row: (String, Option<String>) = m
            .db()
            .query_row(
                "SELECT display_name, fx_json FROM tracks WHERE id = ?",
                [&empty.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        let journaled = count(
            &m,
            "SELECT COUNT(*) FROM oplog WHERE entity_table = 'tracks' AND entity_id = ?",
            &[&empty.id],
        );
        (row, journaled)
    };
    let before = lane_row();

    assert_eq!(recover(&m, &fake_analyze()), 1);

    let after = reload(&m, &practice.id);
    assert_eq!(after.vocal_tracks.len(), 1);
    let take = &after.vocal_tracks[0];
    assert_eq!(take.id, empty.id);
    assert_eq!(take.display_name, "Take 1");
    assert_eq!(take.primary_clip().unwrap().start_ms, 4200);
    assert_eq!(lane_row(), before);
}

#[test]
fn a_crashed_take_is_placed_where_its_stop_would_have_put_it_once_its_backing_started() {
    // The name holds the playhead at REC; the file's head was recorded
    // before the backing rolled, and the take sat late by it.
    let m = Memory::new();
    let practice = create_practice(&m);
    stage_orphan(&m.root, &practice.id, &mono_wav(4800), None);
    recover(&m, &fake_analyze());
    let first = reload(&m, &practice.id).vocal_tracks[0].clone();
    let files = rooted_files(&m.root);

    for (aligned, expected) in [(3650i64, 3650i64), (-120, 0)] {
        let bytes = mono_wav_gain(48000, if aligned < 0 { 0.2 } else { 0.3 });
        let staged = stage_orphan(
            &m.root,
            &practice.id,
            &bytes,
            Some(&format!("{}~{}~4000.wav", new_id(), first.id)),
        );
        assert!(files.write_aligned_start(&text(&staged), aligned));
        assert_eq!(recover(&m, &fake_analyze()), 1);
        let after = reload(&m, &practice.id);
        let clip = after.vocal_tracks.last().unwrap().primary_clip().unwrap();
        assert_eq!(clip.start_ms, expected);
        // Audio from before the timeline's start is cut, as a stop cuts it:
        // kept, the take sat late by it.
        let cut_bytes = if aligned < 0 {
            (-aligned * 48 * 2) as u64
        } else {
            0
        }; // 16-bit mono
        assert_eq!(
            fs::metadata(&clip.audio_file_path).unwrap().len(),
            bytes.len() as u64 - cut_bytes
        );
    }
}

#[test]
fn a_take_whose_cut_was_noted_is_not_cut_again() {
    // The start the cut leaves is written before the cut: a crash after it
    // left the arm's negative estimate beside a cut file, and the next
    // recovery cut it a second time.
    let m = Memory::new();
    let practice = create_practice(&m);
    stage_orphan(&m.root, &practice.id, &mono_wav(4800), None);
    recover(&m, &fake_analyze());
    let first = reload(&m, &practice.id).vocal_tracks[0].clone();
    let bytes = mono_wav_gain(48000, 0.25);
    let staged = stage_orphan(
        &m.root,
        &practice.id,
        &bytes,
        Some(&format!("{}~{}~0.wav", new_id(), first.id)),
    );
    // As a stop (or an earlier recovery) leaves it once its cut is noted.
    assert!(write_aligned_start_to(&text(&staged), 0).wait());
    assert_eq!(recover(&m, &fake_analyze()), 1);
    let after = reload(&m, &practice.id);
    let clip = after.vocal_tracks.last().unwrap().primary_clip().unwrap();
    assert_eq!(clip.start_ms, 0);
    assert_eq!(
        fs::metadata(&clip.audio_file_path).unwrap().len(),
        bytes.len() as u64
    );
}

#[test]
fn a_recovered_take_is_numbered_among_the_takes_of_the_interpretation_it_joins() {
    // Numbered among those on screen, it could repeat a number of its own
    // interpretation's.
    let m = Memory::new();
    let practice = create_practice(&m);
    let other = TakeVersion::new(new_id(), "Other", DateTime::local_ymd(2026, 1, 1));
    let existing = m.root.join("existing.wav");
    write(&existing, &mono_wav_gain(1200, 0.3));
    let existing = text(&existing);
    let active = practice.active_version_id.clone();
    let punched_into = take_over(&existing, "Take 1", Some(&other.id));
    let mut versions = practice.versions.clone();
    versions.push(Arc::new(other.clone()));
    let practice = m
        .song_repo()
        .save_practice(&SongPractice {
            versions,
            ..with_takes(
                &practice,
                vec![
                    take_over(&existing, "Take 1", active.as_deref()),
                    take_over(&existing, "Take 2", active.as_deref()),
                    take_over(&existing, "Take 3", active.as_deref()),
                    punched_into.clone(),
                ],
            )
        })
        .unwrap();

    stage_orphan(
        &m.root,
        &practice.id,
        &mono_wav(4800),
        Some(&format!("{}~{}~500.wav", new_id(), punched_into.id)),
    );
    assert_eq!(recover(&m, &fake_analyze()), 1);
    let after = reload(&m, &practice.id);
    let recovered = after.vocal_tracks.last().unwrap();
    assert_eq!(recovered.version_id.as_deref(), Some(other.id.as_str()));
    assert_eq!(recovered.display_name, "Take 2 (recovered)");
}

#[test]
fn takes_that_crashed_into_one_empty_lane_the_earliest_fills_it() {
    let m = Memory::new();
    let practice = create_practice(&m);
    let empty = save_empty_take(&m, &practice);
    // Named in time order (ULIDs), staged newest first; the directory lists
    // them in an order of its own.
    let names: Vec<String> = (0..8)
        .map(|i| format!("{}~{}~{}.wav", new_id(), empty.id, 1000 * (i + 1)))
        .collect();
    for (i, name) in names.iter().rev().enumerate() {
        stage_orphan(
            &m.root,
            &practice.id,
            &mono_wav_gain(4800, 0.1 + i as f64 * 0.05),
            Some(name),
        );
    }

    assert_eq!(recover(&m, &fake_analyze()), 8);
    let after = reload(&m, &practice.id);
    let filled = after
        .vocal_tracks
        .iter()
        .find(|t| t.id == empty.id)
        .unwrap();
    assert_eq!(filled.primary_clip().unwrap().start_ms, 1000);
}

#[test]
fn a_crashed_take_over_a_take_with_audio_comes_back_as_a_lane_of_its_own() {
    // Where it was recorded.
    let m = Memory::new();
    let practice = create_practice(&m);
    stage_orphan(&m.root, &practice.id, &mono_wav(4800), None);
    recover(&m, &fake_analyze());
    let first = reload(&m, &practice.id).vocal_tracks[0].clone();

    stage_orphan(
        &m.root,
        &practice.id,
        &mono_wav_gain(4800, 0.3),
        Some(&format!("{}~{}~3000.wav", new_id(), first.id)),
    );
    assert_eq!(recover(&m, &fake_analyze()), 1);

    let after = reload(&m, &practice.id);
    assert_eq!(after.vocal_tracks.len(), 2);
    assert_eq!(after.vocal_tracks[0].clips.len(), 1, "untouched");
    let take = after.vocal_tracks.last().unwrap();
    assert_eq!(take.display_name, "Take 2 (recovered)");
    assert_eq!(take.version_id, first.version_id);
    assert_eq!(take.primary_clip().unwrap().start_ms, 3000);
}

#[test]
fn a_recovered_take_is_given_the_colour_after_the_newest_takes() {
    // As the editor gives a new take: it had no colour of its own.
    let m = Memory::new();
    let practice = create_practice(&m);
    let existing = m.root.join("existing.wav");
    write(&existing, &mono_wav_gain(1200, 0.3));
    let existing = text(&existing);
    let [_, ochre, sage, ..] = TAKE_COLORS;
    let active = practice.active_version_id.clone();
    let practice = m
        .song_repo()
        .save_practice(&with_takes(
            &practice,
            vec![AudioTrack {
                color_value: Some(ochre),
                ..take_over(&existing, "Take 1", active.as_deref())
            }],
        ))
        .unwrap();
    stage_orphan(&m.root, &practice.id, &mono_wav(4800), None);
    assert_eq!(recover(&m, &fake_analyze()), 1);
    let after = reload(&m, &practice.id);
    let recovered = after.vocal_tracks.last().unwrap();
    assert_eq!(recovered.display_name, "Take 2 (recovered)");
    assert_eq!(recovered.color_value, Some(sage));
}

#[test]
fn recovery_leaves_every_row_but_the_new_take_as_it_was_stored() {
    // Recovery saved without a base, so the store's own rows stood in as
    // one: a row this build re-encodes differently than it was stored (a
    // field another build wrote) read as a local edit, and was rewritten with
    // the field stripped, and journaled, though nothing touched it.
    let m = Memory::new();
    let practice = create_practice(&m);
    m.db()
        .execute(
            "UPDATE practices SET reverb_json = json_set(COALESCE(reverb_json, '{}'), \
             '$.future', 1), lyrics_json = ? WHERE id = ?",
            [
                r#"[{"text":"la","timeMs":10,"style":"bold"}]"#,
                practice.id.as_str(),
            ],
        )
        .unwrap();
    let row = || -> (Option<String>, Option<String>) {
        m.db()
            .query_row(
                "SELECT reverb_json, lyrics_json FROM practices WHERE id = ?",
                [&practice.id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap()
    };
    let journaled = || {
        count(
            &m,
            "SELECT COUNT(*) FROM oplog WHERE entity_table = 'practices' AND entity_id = ?",
            &[&practice.id],
        )
    };
    let before = row();
    assert!(before.0.as_deref().unwrap().contains("\"future\":1"));
    let journal_before = journaled();

    stage_orphan(&m.root, &practice.id, &mono_wav(4800), None);
    assert_eq!(recover(&m, &fake_analyze()), 1);

    assert_eq!(reload(&m, &practice.id).vocal_tracks.len(), 1);
    assert_eq!(row(), before, "the practice row is untouched");
    assert_eq!(journaled(), journal_before, "and not journaled");
}

#[test]
fn a_recovered_take_past_the_stored_timeline_still_leaves_the_practice_row_as_stored() {
    // Growing the stored timeline to hold the take made the practice row a
    // local edit again: rewritten (another build's fields stripped) and
    // journaled at boot. The editor grows the timeline in memory on open.
    let m = Memory::new();
    let created = create_practice(&m);
    let practice = m
        .song_repo()
        .save_practice(&SongPractice {
            timeline_duration_ms: 20000,
            ..created
        })
        .unwrap();
    m.db()
        .execute(
            "UPDATE practices SET reverb_json = json_set(COALESCE(reverb_json, '{}'), \
             '$.future', 1) WHERE id = ?",
            [&practice.id],
        )
        .unwrap();
    let reverb = || -> Option<String> {
        m.db()
            .query_row(
                "SELECT reverb_json FROM practices WHERE id = ?",
                [&practice.id],
                |r| r.get(0),
            )
            .unwrap()
    };
    let before = reverb();
    assert!(before.as_deref().unwrap().contains("\"future\":1"));
    stage_orphan(&m.root, &practice.id, &mono_wav(4800), None);
    let long: WavAnalyzer = Arc::new(|_| {
        Ok(WaveformResult {
            peaks: vec![0.1, 0.2],
            duration_ms: 600000,
        })
    });
    assert_eq!(recover(&m, &long), 1);
    let after = reload(&m, &practice.id);
    assert_eq!(after.content_end_ms(), 600000);
    assert_eq!(after.timeline_duration_ms, 20000);
    assert_eq!(reverb(), before);
}

#[test]
fn a_second_run_is_a_no_op() {
    let m = Memory::new();
    let practice = create_practice(&m);
    stage_orphan(&m.root, &practice.id, &mono_wav(4800), None);
    assert_eq!(recover(&m, &fake_analyze()), 1);

    assert_eq!(recover(&m, &fake_analyze()), 0);
    assert_eq!(reload(&m, &practice.id).vocal_tracks.len(), 1);
}

#[test]
fn two_byte_identical_staging_files_recover_as_one_take_in_the_same_boot() {
    // The referenced-hash set is read once, before the scan. Without adding
    // each recovery's own hash to it, the second file still looked like
    // bytes nothing had ever saved and became a second "(recovered)" take of
    // the same audio — only the NEXT boot's re-read of the set closed it.
    // Two silent takes of equal length (a muted or unplugged input) produce
    // exactly these bytes.
    let m = Memory::new();
    let practice = create_practice(&m);
    let bytes = mono_wav(4800);
    let first = stage_orphan(&m.root, &practice.id, &bytes, None);
    let second = stage_orphan(&m.root, &practice.id, &bytes, None);

    assert_eq!(recover(&m, &fake_analyze()), 1);
    assert_eq!(reload(&m, &practice.id).vocal_tracks.len(), 1);

    // The recovered one was deleted; the duplicate is left to the sweep.
    assert!(!(first.exists() && second.exists()));
    m.assets().sweep_staging(&HashSet::new(), &HashSet::new());
    assert!(!first.exists());
    assert!(!second.exists());
}

#[test]
fn a_boot_with_many_staged_files_reaches_the_same_outcome_for_each_with_hashes_computed_ahead() {
    // The hashes are prefetched in background threads while the walk runs
    // file by file; every per-file decision must still be the walk's own.
    let m = Memory::new();
    let a = create_practice(&m);
    let b = create_practice(&m);
    let twin = mono_wav_gain(4800, 0.3);
    let twins = [
        stage_orphan(&m.root, &a.id, &twin, None),
        stage_orphan(&m.root, &a.id, &twin, None),
    ];
    let distinct = [
        stage_orphan(&m.root, &b.id, &mono_wav_gain(4800, 0.6), None),
        stage_orphan(&m.root, &b.id, &mono_wav_gain(5200, 0.6), None),
    ];
    let not_a_take = stage_orphan(&m.root, &a.id, &[1, 2, 3, 4, 5, 6, 7, 8], None);
    let orphaned_practice = stage_orphan(&m.root, &new_id(), &mono_wav(4800), None);

    assert_eq!(recover(&m, &fake_analyze()), 3);
    assert_eq!(reload(&m, &a.id).vocal_tracks.len(), 1); // twins: ONE take
    assert_eq!(reload(&m, &b.id).vocal_tracks.len(), 2);
    // Recovered files are deleted; the rest is the sweep's.
    let mut left: Vec<bool> = twins.iter().map(|f| f.exists()).collect();
    left.sort();
    assert_eq!(left, vec![false, true]);
    for f in &distinct {
        assert!(!f.exists());
    }
    assert!(not_a_take.exists());
    assert!(orphaned_practice.exists());
    assert!(m.recovery_retry_paths().is_empty());
    // A second boot finds nothing new to adopt.
    assert_eq!(recover(&m, &fake_analyze()), 0);
}

#[test]
fn a_crash_between_the_row_commit_and_the_file_delete_cannot_duplicate_the_take() {
    // On the next boot.
    let m = Memory::new();
    let practice = create_practice(&m);
    let bytes = mono_wav(4800);
    let orphan = stage_orphan(&m.root, &practice.id, &bytes, None);
    assert_eq!(recover(&m, &fake_analyze()), 1);

    // Simulate the crash window: the take's rows are committed but the
    // staging file survived (recreate the identical bytes at the same path).
    fs::write(&orphan, &bytes).unwrap();
    assert_eq!(recover(&m, &fake_analyze()), 0);
    assert_eq!(reload(&m, &practice.id).vocal_tracks.len(), 1);
    // The skipped leftover is then deleted by the sweep, as before.
    m.assets().sweep_staging(&HashSet::new(), &HashSet::new());
    assert!(!orphan.exists());
}

#[test]
fn a_staged_take_the_pool_already_holds_is_deleted_by_the_sweep_not_kept_in_the_trash() {
    // A graceful stop leaves each take in staging after its save pooled it.
    // The sweep moved all of them to the trash, kept for 30 days, so every
    // recording sat on disk twice for a month.
    let m = Memory::new();
    let practice = create_practice(&m);
    let bytes = mono_wav(4800);
    let saved = stage_orphan(&m.root, &practice.id, &bytes, None);
    assert_eq!(recover(&m, &fake_analyze()), 1);
    fs::write(&saved, &bytes).unwrap(); // the leftover of that save
    let unproven = stage_orphan(&m.root, &practice.id, &[1, 2, 3, 4, 5, 6, 7, 8], None);

    assert_eq!(recover(&m, &fake_analyze()), 0);
    assert_eq!(m.pooled_staging_paths(), set(&[&saved]));
    m.assets()
        .sweep_staging(&m.recovery_retry_paths(), &m.pooled_staging_paths());

    assert!(!saved.exists());
    assert!(!unproven.exists());
    let trashed = trashed_names(&m.assets().trash_dir());
    assert!(trashed.contains(&dart_path::basename(&text(&unproven))));
    assert!(!trashed.contains(&dart_path::basename(&text(&saved))));
    // The take itself is intact in the pool.
    assert_eq!(reload(&m, &practice.id).vocal_tracks.len(), 1);
}

/// The names of every file under [dir], recursively.
fn trashed_names(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(e.file_name().to_string_lossy().into_owned());
            }
        }
    }
    out
}

#[test]
fn a_crash_between_the_pool_import_and_the_row_commit_still_recovers_on_the_retry() {
    // Reusing the half-imported asset.
    let m = Memory::new();
    let practice = create_practice(&m);
    let orphan = stage_orphan(&m.root, &practice.id, &mono_wav(4800), None);
    // Simulate the crash window: the asset row + pooled bytes exist but no
    // clip row ever committed (the save imports first, then writes rows).
    m.assets()
        .import_file(&text(&orphan), AssetStore::DEFAULT_KIND)
        .unwrap();

    assert_eq!(recover(&m, &fake_analyze()), 1);
    let after = reload(&m, &practice.id);
    assert_eq!(after.vocal_tracks.len(), 1);
    // Deduped: the retry reused the existing asset instead of minting a
    // second one for the same bytes.
    assert_eq!(
        count(
            &m,
            "SELECT COUNT(*) FROM assets WHERE deleted_at IS NULL",
            &[]
        ),
        1
    );
    assert!(!orphan.exists());
}

/// Runs the graceful-stop + same-session-delete sequence: saves a take from
/// its staging WAV (the save pools the bytes and deliberately leaves the
/// staging copy for the boot sweep), then deletes the take (tombstoning the
/// clip row while the staging copy survives — delete_audio refuses pool
/// paths). Returns the surviving staging file.
fn save_then_delete_take(m: &Memory, practice: &SongPractice) -> PathBuf {
    let staged = stage_orphan(&m.root, &practice.id, &mono_wav(4800), None);
    let with_take = m
        .song_repo()
        .save_practice(&with_takes(
            practice,
            vec![take_over(
                &text(&staged),
                "Take 1",
                practice.active_version_id.as_deref(),
            )],
        ))
        .unwrap();
    assert!(staged.exists());
    m.song_repo()
        .save_practice(&with_takes(&with_take, Vec::new()))
        .unwrap();
    staged
}

#[test]
fn a_saved_then_deleted_take_is_never_resurrected() {
    // Its staging copy matches a tombstoned reference.
    let m = Memory::new();
    let practice = create_practice(&m);
    let staged = save_then_delete_take(&m, &practice);

    // Next boot: recovery must NOT adopt the leftover as a "(recovered)"
    // take — the bytes match a reference that was deliberately deleted.
    assert_eq!(recover(&m, &fake_analyze()), 0);
    assert!(reload(&m, &practice.id).vocal_tracks.is_empty());
    // The leftover is the sweep's to reclaim, exactly as before.
    m.assets().sweep_staging(&HashSet::new(), &HashSet::new());
    assert!(!staged.exists());
}

#[test]
fn a_deleted_take_stays_deleted_even_after_the_gc_tombstoned_its_asset_and_reclaimed_its_file() {
    let m = Memory::new();
    let practice = create_practice(&m);
    save_then_delete_take(&m, &practice);
    // Settle the asset's journal entry as a push ack would (the ack
    // hard-deletes): the sweep holds back any asset whose entry is still
    // pending, since a pushed-but-unacked row may already be server-side.
    m.db()
        .execute("DELETE FROM oplog WHERE entity_table = 'assets'", [])
        .unwrap();
    // The GC pass a later boot would run: the take's asset is unreferenced
    // by live rows, so it is tombstoned and its pooled bytes reclaimed.
    assert_eq!(m.assets().sweep_unreferenced(Duration::ZERO).unwrap(), 1);

    assert_eq!(recover(&m, &fake_analyze()), 0);
    assert!(reload(&m, &practice.id).vocal_tracks.is_empty());
}

#[test]
fn files_under_the_edits_subdirectory_are_invisible_to_recovery_but_swept() {
    // Split/concat/punch-in outputs stage under audio/<pid>/edits/ exactly so
    // a kill between the file write and the save commit cannot become a
    // phantom "(recovered)" take — recovery only adopts files DIRECTLY under
    // audio/<pid>/, while the staging sweep reclaims the whole tree.
    let m = Memory::new();
    let practice = create_practice(&m);
    let f = m
        .root
        .join("audio")
        .join(&practice.id)
        .join("edits")
        .join(format!("{}.wav", new_id()));
    write(&f, &mono_wav(4800));

    assert_eq!(recover(&m, &fake_analyze()), 0);
    assert!(reload(&m, &practice.id).vocal_tracks.is_empty());
    assert!(f.exists()); // untouched by recovery...
    m.assets().sweep_staging(&HashSet::new(), &HashSet::new());
    assert!(!f.exists()); // ...reclaimed by the boot sweep
}

#[test]
fn a_failed_recovery_attempt_is_exempted_from_this_boots_sweep_and_retried_next_boot() {
    let m = Memory::new();
    let practice = create_practice(&m);
    let failing = stage_orphan(&m.root, &practice.id, &mono_wav(4800), None);
    let junk = stage_orphan(&m.root, &practice.id, &(0..64u8).collect::<Vec<_>>(), None);

    // A systemic failure (broken decoder, disk error) — NOT per-file junk.
    assert_eq!(recover(&m, &broken_analyze()), 0);
    assert_eq!(m.recovery_retry_paths(), set(&[&failing]));

    // The boot sweep spares exactly the failed attempt; ordinary junk goes.
    m.assets()
        .sweep_staging(&m.recovery_retry_paths(), &HashSet::new());
    assert!(failing.exists());
    assert!(!junk.exists());

    // Next boot, healthy decoder: the exempted take is recovered normally.
    assert_eq!(recover(&m, &fake_analyze()), 1);
    assert_eq!(reload(&m, &practice.id).vocal_tracks.len(), 1);
}

#[test]
fn a_recording_whose_clip_an_edit_replaced_is_not_recovered_again() {
    // A graceful stop pools the take and leaves its staging WAV for the boot
    // sweep, which decides "already saved" by asking whether any row — live
    // or tombstoned — still references those bytes. Split and merge replace
    // the edited clip with a NEW row, so the old one is tombstoned with its
    // asset intact and the original stays referenced. (Re-pointing the same
    // clip id at the edited file overwrote that reference, and the original
    // came back as a whole "(recovered)" take.)
    let m = Memory::new();
    let practice = create_practice(&m);
    let rec = stage_orphan(&m.root, &practice.id, &mono_wav(4800), None);
    let version = practice.active_version().map(|v| v.id.clone());
    m.song_repo()
        .save_practice(&with_takes(
            &practice,
            vec![take_over(&text(&rec), "Take 1", version.as_deref())],
        ))
        .unwrap();

    let edited = m.root.join("edits").join(format!("{}.wav", new_id()));
    write(&edited, &mono_wav_gain(2400, 0.3));
    let loaded = reload(&m, &practice.id);
    let lane = (*loaded.vocal_tracks[0]).clone();
    m.song_repo()
        .save_practice(&with_takes(
            &loaded,
            vec![AudioTrack {
                clips: vec![AudioClip::new(new_id(), text(&edited), 50)],
                ..lane
            }],
        ))
        .unwrap();

    assert_eq!(recover(&m, &fake_analyze()), 0);
    assert_eq!(reload(&m, &practice.id).vocal_tracks.len(), 1);
}

#[test]
fn an_unreadable_take_is_held_back_for_retry_not_classified_as_junk() {
    // probe_wav answers None for SHAPE only. When it could not read the file
    // at all, answering "not a recording" would sweep the only copy of a
    // crashed take into the trash on the strength of a read that never
    // happened — and the trash is never rescanned, so the 30-day prune
    // finishes the job.
    //
    // Only a permission bit makes a real file in the staging tree unreadable
    // here, and root reads it anyway (as the Dart test, skipped then).
    if runs_as_root() {
        eprintln!("skipped: root reads a file whatever its permission bits");
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let m = Memory::new();
    let practice = create_practice(&m);
    let unreadable = stage_orphan(&m.root, &practice.id, &mono_wav(4800), None);
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();

    assert_eq!(recover(&m, &fake_analyze()), 0);
    assert_eq!(
        m.recovery_retry_paths(),
        set(&[&unreadable]),
        "an I/O failure is an environmental failure, not junk"
    );

    m.assets()
        .sweep_staging(&m.recovery_retry_paths(), &HashSet::new());
    assert!(unreadable.exists(), "spared by the sweep");

    // Readable again next boot: recovered normally.
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(recover(&m, &fake_analyze()), 1);
    assert_eq!(reload(&m, &practice.id).vocal_tracks.len(), 1);
}

#[test]
fn a_recovery_whose_save_failed_is_exempted_too_not_just_a_thrown_one() {
    let m = Memory::new();
    let practice = create_practice(&m);
    let failing = stage_orphan(&m.root, &practice.id, &mono_wav(4800), None);

    // The store reports failure as an error, never a panic: block the pool
    // tree (a regular file where the `assets/` directory must be) so the
    // save's asset import cannot land. Without this case being treated as
    // an environmental failure, the only copy of the take goes to the sweep
    // and no later boot ever retries it.
    let blocker = m.root.join("assets");
    fs::write(&blocker, "not a directory").unwrap();

    assert_eq!(recover(&m, &fake_analyze()), 0);
    assert_eq!(m.recovery_retry_paths(), set(&[&failing]));

    m.assets()
        .sweep_staging(&m.recovery_retry_paths(), &HashSet::new());
    assert!(failing.exists());

    // Next boot, healthy disk: the exempted take is recovered normally.
    fs::remove_file(&blocker).unwrap();
    assert_eq!(recover(&m, &fake_analyze()), 1);
    assert_eq!(reload(&m, &practice.id).vocal_tracks.len(), 1);
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

#[test]
fn a_staged_file_that_has_kept_failing_for_over_7_days_is_released_to_the_sweep_after_all() {
    let m = Memory::new();
    let practice = create_practice(&m);
    let stale = stage_orphan(&m.root, &practice.id, &mono_wav(4800), None);
    let stamp = PathBuf::from(format!("{}.retry", text(&stale)));
    let first_failure = now_ms() - 8 * DAY_MS;
    // Two boots failed it already; this is the third.
    fs::write(&stamp, format!("{first_failure} 2")).unwrap();

    assert_eq!(recover(&m, &broken_analyze()), 0);
    // Too old to keep protecting: the exemption lapses and the sweep takes
    // it (the trash still holds it for its own retention window).
    assert!(m.recovery_retry_paths().is_empty());
    m.assets()
        .sweep_staging(&m.recovery_retry_paths(), &HashSet::new());
    assert!(!stale.exists());
    assert!(!stamp.exists());
}

#[test]
fn a_crash_left_untouched_for_over_7_days_still_gets_its_retries() {
    // The window opens at its first failed attempt. The app was not opened
    // for a week after the crash; the first boot's attempt then failed (disk
    // full, decoder down). Measured from the file's modification time, the
    // take went to the sweep on that very first attempt.
    let m = Memory::new();
    let practice = create_practice(&m);
    let old = stage_orphan(&m.root, &practice.id, &mono_wav(4800), None);
    fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(8 * 24 * 60 * 60))
        .unwrap();

    assert_eq!(recover(&m, &broken_analyze()), 0);
    assert_eq!(m.recovery_retry_paths(), set(&[&old]));
    m.assets()
        .sweep_staging(&m.recovery_retry_paths(), &HashSet::new());
    assert!(old.exists());
    // The first failure's time stays with it for the next boot's verdict.
    let stamp = PathBuf::from(format!("{}.retry", text(&old)));
    assert!(stamp.exists());
    let stamped = fs::read_to_string(&stamp).unwrap();
    let first_ms: i64 = stamped.split(' ').next().unwrap().parse().unwrap();
    assert!(now_ms() - first_ms < 60 * 60 * 1000);

    // A later failure measures from that first one, not from now.
    assert_eq!(recover(&m, &broken_analyze()), 0);
    assert_eq!(fs::read_to_string(&stamp).unwrap(), format!("{first_ms} 2"));
    assert_eq!(m.recovery_retry_paths(), set(&[&old]));

    // Healthy again: recovered normally.
    assert_eq!(recover(&m, &fake_analyze()), 1);
    assert_eq!(reload(&m, &practice.id).vocal_tracks.len(), 1);
}

#[test]
fn a_file_that_failed_once_and_then_waited_out_the_week_still_gets_more_attempts() {
    // The window alone, measured from the first failure, released the take
    // on its second attempt when the app then went unopened for a week.
    let m = Memory::new();
    let practice = create_practice(&m);
    let staged = stage_orphan(&m.root, &practice.id, &mono_wav(4800), None);
    let stamp = PathBuf::from(format!("{}.retry", text(&staged)));
    let first_failure = now_ms() - 8 * DAY_MS;
    fs::write(&stamp, format!("{first_failure} 1")).unwrap();

    assert_eq!(recover(&m, &broken_analyze()), 0);
    assert_eq!(m.recovery_retry_paths(), set(&[&staged]));
    assert_eq!(
        fs::read_to_string(&stamp).unwrap(),
        format!("{first_failure} 2")
    );
}

#[test]
fn a_stamp_that_cannot_be_read_or_lies_in_the_future_starts_over() {
    let m = Memory::new();
    let practice = create_practice(&m);
    for (i, stamped) in ["garbage", &format!("{} 9", now_ms() + 10 * DAY_MS)]
        .into_iter()
        .enumerate()
    {
        let staged = stage_orphan(
            &m.root,
            &practice.id,
            &mono_wav_gain(4800, 0.1 + i as f64 * 0.2),
            None,
        );
        let stamp = PathBuf::from(format!("{}.retry", text(&staged)));
        fs::write(&stamp, stamped).unwrap();
        assert_eq!(recover(&m, &broken_analyze()), 0);
        assert!(m.recovery_retry_paths().contains(&text(&staged)));
        let rewritten = fs::read_to_string(&stamp).unwrap();
        let mut parts = rewritten.split(' ');
        let first: i64 = parts.next().unwrap().parse().unwrap();
        assert!((now_ms() - first).abs() < 60 * 60 * 1000, "{rewritten}");
        assert_eq!(parts.next(), Some("1"), "{rewritten}");
        fs::remove_file(&staged).unwrap();
        fs::remove_file(&stamp).unwrap();
    }
}

#[test]
fn a_recorded_hash_does_not_speak_for_a_file_that_grew_since_its_import() {
    // A power cut rolls the staging file back, never forward: a longer file
    // was written to after the import, and those bytes were never saved.
    let m = Memory::new();
    let practice = create_practice(&m);
    let staged = stage_orphan(&m.root, &practice.id, &mono_wav(2400), None);
    let version = practice.active_version_id.clone();
    m.song_repo()
        .save_practice(&with_takes(
            &practice,
            vec![take_over(&text(&staged), "Take 1", version.as_deref())],
        ))
        .unwrap();
    fs::write(&staged, mono_wav(4800)).unwrap();

    assert_eq!(recover(&m, &fake_analyze()), 1);
    assert_eq!(reload(&m, &practice.id).vocal_tracks.len(), 2);
}

#[test]
fn a_take_whose_staging_copy_lost_its_last_writes_to_a_power_cut_is_not_recovered_twice() {
    // Graceful stop: the save pools the take and leaves the staging copy for
    // the boot sweep, recording the hash it pooled beside it.
    let m = Memory::new();
    let practice = create_practice(&m);
    let staged = stage_orphan(&m.root, &practice.id, &mono_wav(4800), None);
    let version = practice.active_version_id.clone();
    m.song_repo()
        .save_practice(&with_takes(
            &practice,
            vec![take_over(&text(&staged), "Take 1", version.as_deref())],
        ))
        .unwrap();
    assert!(PathBuf::from(format!("{}.sha256", text(&staged))).exists());

    // The power cut: the recorder never flushed the staging file, so it
    // comes back holding only what reached the disk — older, shorter bytes
    // whose own hash matches nothing saved.
    fs::write(&staged, mono_wav(2400)).unwrap();

    assert_eq!(recover(&m, &fake_analyze()), 0);
    assert_eq!(reload(&m, &practice.id).vocal_tracks.len(), 1);
    m.assets().sweep_staging(&HashSet::new(), &HashSet::new());
    assert!(!staged.exists());
}

#[test]
fn a_take_pooled_but_not_saved_when_the_power_went_is_recovered_whole() {
    // The save pooled the take (its file, its asset row) but the power went
    // before its clip rows committed; the recorder never flushed the staging
    // file, so it comes back shorter. The take comes back whole, from the
    // pooled copy, and that copy is its asset, not an orphan to trash.
    let m = Memory::new();
    let practice = create_practice(&m);
    let full = mono_wav(4800);
    let staged = stage_orphan(&m.root, &practice.id, &full, None);
    m.assets()
        .import_file(&text(&staged), AssetStore::DEFAULT_KIND)
        .unwrap();
    fs::write(&staged, mono_wav(2400)).unwrap();

    let before = reload(&m, &practice.id).vocal_tracks.len();
    assert_eq!(recover(&m, &fake_analyze()), 1);
    let reloaded = reload(&m, &practice.id);
    assert_eq!(reloaded.vocal_tracks.len(), before + 1);
    let clip = &reloaded.vocal_tracks.last().unwrap().clips[0];
    assert_eq!(
        fs::read(&clip.audio_file_path).unwrap(),
        full,
        "recovered from the pooled copy"
    );
    assert_eq!(m.assets().sweep_orphan_pool_files().unwrap(), 0);
}

#[test]
fn a_held_back_recording_keeps_its_recorded_hash_through_the_sweep() {
    // The hash a save recorded is what tells a power-cut leftover from an
    // orphan (see above); a boot whose attempt failed must not sweep it away
    // from the file it describes.
    let m = Memory::new();
    let practice = create_practice(&m);
    let staged = stage_orphan(&m.root, &practice.id, &mono_wav(4800), None);
    m.assets()
        .import_file(&text(&staged), AssetStore::DEFAULT_KIND)
        .unwrap();
    let sidecar = PathBuf::from(format!("{}.sha256", text(&staged)));
    assert!(sidecar.exists());

    assert_eq!(recover(&m, &broken_analyze()), 0);
    m.assets()
        .sweep_staging(&m.recovery_retry_paths(), &HashSet::new());
    assert!(staged.exists());
    assert!(sidecar.exists());
}

#[test]
fn numbering_continues_after_existing_takes_in_the_interpretation() {
    let m = Memory::new();
    let practice = create_practice(&m);
    // An already-saved ordinary take named by the normal scheme.
    let existing = m.root.join("existing.wav");
    write(&existing, &mono_wav_gain(1200, 0.3));
    let version = practice.active_version_id.clone();
    let practice = m
        .song_repo()
        .save_practice(&with_takes(
            &practice,
            vec![take_over(&text(&existing), "Take 3", version.as_deref())],
        ))
        .unwrap();

    stage_orphan(&m.root, &practice.id, &mono_wav(4800), None);
    assert_eq!(recover(&m, &fake_analyze()), 1);
    let after = reload(&m, &practice.id);
    let names: Vec<&str> = after
        .vocal_tracks
        .iter()
        .map(|t| t.display_name.as_str())
        .collect();
    assert!(names.contains(&"Take 3"), "{names:?}");
    assert!(names.contains(&"Take 4 (recovered)"), "{names:?}");
}

#[test]
fn an_orphan_of_a_deleted_or_unknown_practice_is_left_for_the_sweep() {
    let m = Memory::new();
    let practice = create_practice(&m);
    m.song_repo().delete_practice(&practice.id).unwrap();
    let tombstoned = stage_orphan(&m.root, &practice.id, &mono_wav(4800), None);
    let unknown = stage_orphan(&m.root, "no-such-practice", &mono_wav(4800), None);

    assert_eq!(recover(&m, &fake_analyze()), 0);
    assert!(tombstoned.exists()); // untouched, then swept
    assert!(unknown.exists());
    m.assets().sweep_staging(&HashSet::new(), &HashSet::new());
    assert!(!tombstoned.exists());
    assert!(!unknown.exists());
}

#[test]
fn empty_unparseable_and_non_recording_files_are_never_recovered() {
    let m = Memory::new();
    let practice = create_practice(&m);
    // Killed before the first header patch: valid WAV, zero data bytes.
    let empty = stage_orphan(&m.root, &practice.id, &mono_wav(0), None);
    // Not a WAV at all.
    let garbage = stage_orphan(&m.root, &practice.id, &(0..128u8).collect::<Vec<_>>(), None);
    // A stereo file (an import, not something the recorder writes): flip the
    // channel-count field of an otherwise valid header.
    let mut stereo_bytes = mono_wav(4800);
    stereo_bytes[22..24].copy_from_slice(&2u16.to_le_bytes());
    let stereo = stage_orphan(&m.root, &practice.id, &stereo_bytes, None);

    assert_eq!(recover(&m, &fake_analyze()), 0);
    assert!(reload(&m, &practice.id).vocal_tracks.is_empty());
    for f in [&empty, &garbage, &stereo] {
        assert!(f.exists()); // untouched, then swept
    }
    m.assets().sweep_staging(&HashSet::new(), &HashSet::new());
    for f in [&empty, &garbage, &stereo] {
        assert!(!f.exists());
    }
}

#[test]
fn an_import_cut_off_before_its_save_is_not_adopted_as_a_take_and_still_pools_and_sweeps() {
    // A mono 48 kHz WAV with the canonical header is exactly the recorder's
    // shape, and ensure_streamable_wav passes it through unchanged, so the
    // staged copy IS the track's file until the save pools it. A quit or
    // crash in that window (the quit does not wait for an import) left it
    // where recovery adopts orphans: the next boot showed it as
    // "Take N (recovered)".
    let m = Memory::new();
    let practice = create_practice(&m);
    let ext = tempfile::tempdir().unwrap();
    let src = ext.path().join("guide vocal.wav");
    fs::write(&src, mono_wav(48000)).unwrap();
    let files = rooted_files(&m.root);

    let staged = files.import_audio(&practice.id, &text(&src)).unwrap();
    assert!(Path::new(&staged).exists());
    assert!(
        dart_path::is_within(&text(&m.root.join("audio").join(&practice.id)), &staged),
        "imports still stage inside the practice tree"
    );

    // The app quits here, before the save. Next boot:
    assert_eq!(recover(&m, &fake_analyze()), 0);
    assert!(reload(&m, &practice.id).vocal_tracks.is_empty());
    assert!(m.recovery_retry_paths().is_empty());

    // Had the save run instead, it pools the staged file as usual.
    let pooled = files.import_audio(&practice.id, &text(&src)).unwrap();
    let version = practice.active_version().map(|v| v.id.clone());
    m.song_repo()
        .save_practice(&with_takes(
            &practice,
            vec![AudioTrack {
                version_id: version,
                ..AudioTrack::new(new_id(), AudioTrackKind::Vocal, "Guide", DateTime::now())
                    .with_audio(pooled.clone(), 0, 1000, vec![0.1])
            }],
        ))
        .unwrap();
    let after = reload(&m, &practice.id);
    let clip = after.vocal_tracks[0].primary_clip().unwrap();
    assert!(dart_path::is_within(
        &text(&m.root.join("assets")),
        &clip.audio_file_path
    ));

    // And the boot sweep still reclaims every staged import.
    m.assets().sweep_staging(&HashSet::new(), &HashSet::new());
    assert!(!Path::new(&staged).exists());
    assert!(!Path::new(&pooled).exists());
}
