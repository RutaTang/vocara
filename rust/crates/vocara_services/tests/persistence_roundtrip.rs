//! Port of `client/integration_test/persistence_roundtrip_test.dart`:
//! persistence round trips through the real stack (`Persistence::memory()`:
//! the SQLite library, the content-addressed asset pool and the
//! repositories), with WAV fixtures.
//!
//! The first Dart test drove the song screen's view model through a full
//! edit session, then opened a FRESH view model on the same store and
//! checked every field. The view model lives in `vocara_app` (a crate above
//! this one), so here the session is driven through what the view model
//! drives: the import pipeline it runs per file (`AudioFileService`'s copy
//! into the sandbox, `AudioEditService`'s transcode to an engine-streamable
//! WAV, the waveform analysis) and the repository's three-way save it makes
//! per edit (holding the loaded base hashes and swapping in each save's), the
//! edits built as the view model builds them (`add_note`, `add_technique`,
//! the mixer's FX / reverb / master commits, `parse_plain_lyrics`, a new
//! interpretation then a switch back). The fresh view model is a fresh
//! `load_practice` of the same store, which is what its load runs; the
//! assertions are Dart's, on the model fields the view model shows (its
//! `lyrics`, `mixer.reverbSettings`, `mixer.masterVolume`, `versions.*`
//! read straight off the loaded practice).
//!
//! What needs the audio engine runs only where it exists: with the `device`
//! feature (macOS) the import transcodes the 44.1 kHz fixtures to the
//! engine rate and the analysis decodes through the engine bridge, as Dart's
//! did through AVFoundation and just_waveform. Elsewhere the copy stands as
//! the track's file and the analysis decodes the PCM WAV in Rust
//! (`read_wav_mono`) before the same `analyze_pcm` the engine path ends in —
//! so the round trip itself (peaks, durations, files, every field) runs on
//! every platform. Dart's `MediaKit.ensureInitialized` and the view model's
//! other collaborators (playback, spectrogram, separation, settings) have no
//! part in what is persisted and are not involved.

use std::f64::consts::PI;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use vocara_model::models::audio_fx::{ReverbSettings, TrackFx};
use vocara_model::models::audio_track::{AudioClip, AudioTrack, AudioTrackKind};
use vocara_model::models::practice_note::PracticeNote;
use vocara_model::models::song_practice::SongPractice;
use vocara_model::models::take_version::TakeVersion;
use vocara_model::models::technique_marker::TechniqueMarker;
use vocara_model::util::dart_date_time::DateTime;
use vocara_model::util::dart_path;
use vocara_model::util::id::new_id;
use vocara_model::util::lyrics_format::parse_plain_lyrics;
use vocara_services::audio_edit_service::AudioEditService;
use vocara_services::audio_file_service::AudioFileService;
use vocara_services::local_storage_service::LocalStorageService;
use vocara_services::persistence::Persistence;
use vocara_services::waveform_service::{WaveformResult, DEFAULT_TARGET_BUCKETS};
use vocara_store::db::rows::AssetRow;
use vocara_store::db::AssetStore;
use vocara_store::repositories::SongPracticeRepository;
use vocara_store::sync::sync_hashes::Loaded;

/// Writes a mono 16-bit PCM WAV sine tone at [freq] Hz and returns its path.
/// Each distinct ([freq], [seconds]) pair yields distinct bytes, so two
/// writes with the same parameters are byte-identical (drives the dedup
/// assertions).
fn write_sine_wav(path: &Path, seconds: u32, freq: f64) -> String {
    const SAMPLE_RATE: u32 = 44100;
    let samples = SAMPLE_RATE * seconds;
    const BYTES_PER_SAMPLE: u32 = 2;
    let data_size = samples * BYTES_PER_SAMPLE;
    let mut bytes = Vec::with_capacity(44 + data_size as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_size).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
    bytes.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    bytes.extend_from_slice(&(SAMPLE_RATE * BYTES_PER_SAMPLE).to_le_bytes()); // byte rate
    bytes.extend_from_slice(&(BYTES_PER_SAMPLE as u16).to_le_bytes()); // block align
    bytes.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_size.to_le_bytes());
    for i in 0..samples {
        // Dart's `(x).round()`: half away from zero, as f64::round.
        let value =
            ((2.0 * PI * freq * f64::from(i) / f64::from(SAMPLE_RATE)).sin() * 0.5 * 32767.0)
                .round() as i16;
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
    path.to_string_lossy().into_owned()
}

/// A temp directory and an in-memory store whose pool is removed with it.
struct Store {
    persistence: Persistence,
    tmp: tempfile::TempDir,
}

impl Store {
    fn new(prefix: &str) -> Store {
        Store {
            persistence: Persistence::memory().unwrap(),
            tmp: tempfile::Builder::new().prefix(prefix).tempdir().unwrap(),
        }
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        // The pool `Persistence::memory()` made is the caller's to remove.
        let _ = fs::remove_dir_all(self.persistence.assets().root());
    }
}

/// The waveform analysis the view model runs on an imported file.
#[cfg(feature = "device")]
fn analyze(path: &str) -> WaveformResult {
    vocara_services::waveform_service::analyze(path, DEFAULT_TARGET_BUCKETS).unwrap()
}

/// The waveform analysis the view model runs on an imported file, its
/// decode done in Rust (no engine on this platform): the PCM WAV read, then
/// the analysis the engine's decode feeds.
#[cfg(not(feature = "device"))]
fn analyze(path: &str) -> WaveformResult {
    let pcm =
        vocara_services::audio_dsp::read_wav_mono(&fs::read(path).unwrap()).expect("a PCM WAV");
    vocara_services::waveform_service::analyze_pcm(
        path,
        &pcm.samples,
        u32::try_from(pcm.sample_rate).unwrap(),
        DEFAULT_TARGET_BUCKETS,
    )
    .unwrap()
}

/// The view model's import of [source] for [practice_id]: the copy into
/// the sandbox, the transcode to an engine-streamable WAV (the copy is
/// dropped when one was made) and the analysis. Returns the track's file
/// and its analysis.
fn import(
    files: &AudioFileService,
    edits: &AudioEditService,
    practice_id: &str,
    source: &str,
) -> (String, WaveformResult) {
    let imported = files.import_audio(practice_id, source).unwrap();
    let dest = if cfg!(feature = "device") {
        let dest = edits.ensure_streamable_wav(practice_id, &imported).unwrap();
        if dest != imported {
            files.delete_audio(&imported).unwrap();
        }
        dest
    } else {
        // No engine to transcode with: the copy stands as the track's file.
        let _ = edits;
        imported
    };
    let analysis = analyze(&dest);
    (dest, analysis)
}

/// An editor over one practice, as the view model holds it: the aggregate
/// (`practice`) and the base hashes of its last load or save (`held`).
/// Every edit is one three-way save.
struct Editor<'a> {
    repo: &'a dyn SongPracticeRepository,
    practice: SongPractice,
    held: Loaded<SongPractice>,
}

impl<'a> Editor<'a> {
    fn open(repo: &'a dyn SongPracticeRepository, id: &str) -> Editor<'a> {
        let held = repo.load_practice(id).unwrap();
        Editor {
            repo,
            practice: held.aggregate.clone(),
            held,
        }
    }

    fn save(&mut self, next: SongPractice) {
        let saved = self
            .repo
            .save_practice_with_base(&next, &self.held.base_hashes)
            .unwrap();
        self.practice = saved.aggregate.clone();
        self.held = saved;
    }

    /// `update_track`: [f] applied to the lane [track_id] (a backing or a
    /// take), then saved.
    fn update_track(&mut self, track_id: &str, f: impl Fn(&AudioTrack) -> AudioTrack) {
        let p = self.practice.clone();
        let map = |t: &Arc<AudioTrack>| {
            if t.id == track_id {
                Arc::new(f(t))
            } else {
                t.clone()
            }
        };
        self.save(SongPractice {
            accompaniment: p.accompaniment.as_ref().map(map),
            reference: p.reference.as_ref().map(map),
            vocal_tracks: p.vocal_tracks.iter().map(map).collect(),
            ..p.clone()
        });
    }

    /// `add_note`: a note at [start_ms] (a point) or across
    /// [start_ms]..[end_ms], kept in position order.
    fn add_note(&mut self, track_id: &str, start_ms: i64, end_ms: Option<i64>, content: &str) {
        let now = DateTime::now();
        let note = PracticeNote {
            id: new_id(),
            position_ms: start_ms,
            end_ms,
            content: content.to_string(),
            created_at: now,
            updated_at: now,
        };
        self.update_track(track_id, |t| {
            let mut notes = t.notes.clone();
            notes.push(note.clone());
            notes.sort_by_key(|n| n.position_ms);
            AudioTrack { notes, ..t.clone() }
        });
    }

    /// `add_technique`: a marker of [technique_id] at [start_ms] (a point)
    /// or across [start_ms]..[end_ms], kept in position order.
    fn add_technique(
        &mut self,
        track_id: &str,
        technique_id: &str,
        technique_name: &str,
        start_ms: i64,
        end_ms: Option<i64>,
    ) {
        let now = DateTime::now();
        let marker = TechniqueMarker {
            id: new_id(),
            position_ms: start_ms,
            end_ms,
            technique_id: technique_id.to_string(),
            technique_name: technique_name.to_string(),
            created_at: now,
            updated_at: now,
        };
        self.update_track(track_id, |t| {
            let mut markers = t.technique_markers.clone();
            markers.push(marker.clone());
            markers.sort_by_key(|m| m.position_ms);
            AudioTrack {
                technique_markers: markers,
                ..t.clone()
            }
        });
    }
}

#[test]
fn a_full_edit_session_then_every_field_round_trips_through_a_fresh_load_of_the_same_store() {
    let store = Store::new("vocara_roundtrip");
    let tmp = store.tmp.path();
    let stamp = DateTime::now().micros_since_epoch();
    let backing_wav = write_sine_wav(&tmp.join(format!("backing_{stamp}.wav")), 1, 220.0);
    let take_wav = write_sine_wav(&tmp.join(format!("take_{stamp}.wav")), 1, 440.0);

    let persistence = &store.persistence;
    let repo: &dyn SongPracticeRepository = persistence.song_repo().as_ref();
    let files = Arc::new(AudioFileService::new(Arc::new(
        LocalStorageService::with_root(tmp.join("app")),
    )));
    let edits = AudioEditService::new(files.clone());

    let created = repo.create_practice("Round-trip song").unwrap();
    let practice_id = created.id.clone();
    let version_id = created
        .active_version_id
        .clone()
        .expect("a new practice has an active interpretation");

    // --- Drive the session as the view model does. ---
    let mut vm = Editor::open(repo, &practice_id);

    // Accompaniment import (real waveform analysis on a real WAV).
    let (dest, analysis) = import(&files, &edits, &practice_id, &backing_wav);
    let backing = AudioTrack::new(
        new_id(),
        AudioTrackKind::Accompaniment,
        dart_path::basename_without_extension(&backing_wav),
        DateTime::now(),
    )
    .with_audio(dest, 0, analysis.duration_ms, analysis.peaks);
    let next = SongPractice {
        accompaniment: Some(Arc::new(backing)),
        ..vm.practice.clone()
    };
    vm.save(next);
    let accomp = vm.practice.accompaniment.clone().expect("an accompaniment");
    assert!(!accomp.primary_clip().unwrap().waveform_peaks.is_empty());
    assert!(accomp.duration_ms() > 500);

    // A vocal take (its own lane) in the active interpretation.
    let (dest, analysis) = import(&files, &edits, &practice_id, &take_wav);
    let take = AudioTrack {
        version_id: vm.practice.active_version().map(|v| v.id.clone()),
        ..AudioTrack::new(
            new_id(),
            AudioTrackKind::Vocal,
            dart_path::basename_without_extension(&take_wav),
            DateTime::now(),
        )
        .with_audio(dest, 0, analysis.duration_ms, analysis.peaks)
    };
    let mut takes = vm.practice.vocal_tracks.clone();
    takes.push(Arc::new(take));
    let next = SongPractice {
        vocal_tracks: takes,
        ..vm.practice.clone()
    };
    vm.save(next);
    let vocal_takes = vm.practice.active_vocal_tracks();
    assert_eq!(vocal_takes.len(), 1);
    let take_id = vocal_takes[0].id.clone();
    assert!(!vocal_takes[0]
        .primary_clip()
        .unwrap()
        .waveform_peaks
        .is_empty());

    // Notes at the playhead on both a backing and a vocal lane.
    let playhead_ms = 250;
    vm.add_note(&accomp.id, playhead_ms, None, "count-in here");
    vm.add_note(&take_id, 400, Some(900), "sustain this phrase");
    assert_eq!(vm.practice.accompaniment.as_ref().unwrap().notes.len(), 1);
    assert_eq!(
        vm.practice.active_vocal_tracks()[0].notes.len(),
        1,
        "the take note must be on the take lane"
    );

    // Technique markers on the take (a point + a range).
    vm.add_technique(&take_id, "tech-twang", "Twang", 500, None);
    vm.add_technique(&take_id, "tech-cry", "Cry", 600, Some(1200));
    assert_eq!(
        vm.practice.active_vocal_tracks()[0].technique_markers.len(),
        2
    );

    // Track FX (EQ on, a boosted band) committed. FX is addressed by track
    // id so no selection is needed.
    let edited_fx = TrackFx {
        eq_on: true,
        comp_on: true,
        send_db: -6.0,
        ..TrackFx::flat()
    };
    let mut band = edited_fx.eq[2];
    band.gain_db = 4.5;
    let boosted_fx = edited_fx.with_band(2, band);
    vm.update_track(&take_id, |t| AudioTrack {
        fx: Some(boosted_fx.clone()),
        ..t.clone()
    });

    // Shared reverb tuned + committed.
    let reverb = ReverbSettings {
        room_size: 0.8,
        damping: 0.3,
        wet: 0.42,
        width: 0.9,
    };
    let next = SongPractice {
        reverb: Some(reverb),
        ..vm.practice.clone()
    };
    vm.save(next);

    // Master volume.
    let next = SongPractice {
        master_volume: 0.66,
        ..vm.practice.clone()
    };
    vm.save(next);

    // Lyrics.
    let next = SongPractice {
        lyrics: parse_plain_lyrics("first line\nsecond line\nthird line"),
        ..vm.practice.clone()
    };
    vm.save(next);
    assert_eq!(vm.practice.lyrics.len(), 3);

    // A second interpretation, then switch back to the first so the active
    // version is deterministic for the reload assertions.
    let powerful = TakeVersion::new(new_id(), "Powerful", DateTime::now());
    let mut versions = vm.practice.versions.clone();
    versions.push(Arc::new(powerful.clone()));
    let next = SongPractice {
        versions,
        active_version_id: Some(powerful.id.clone()),
        ..vm.practice.clone()
    };
    vm.save(next);
    assert_eq!(vm.practice.versions.len(), 2);
    let next = SongPractice {
        active_version_id: Some(version_id.clone()),
        ..vm.practice.clone()
    };
    vm.save(next);
    assert_eq!(
        vm.practice.active_version().map(|v| v.id.as_str()),
        Some(version_id.as_str())
    );

    // Capture expected values before tearing the editor down.
    let expected_accomp_peaks = accomp.primary_clip().unwrap().waveform_peaks.len();
    let expected_take_peaks = vm.practice.active_vocal_tracks()[0]
        .primary_clip()
        .unwrap()
        .waveform_peaks
        .len();
    let expected_timeline = vm.practice.timeline_duration_ms;
    drop(vm);

    // --- A fresh load of the SAME store + practiceId: everything must
    // reload. ---
    let vm2 = Editor::open(repo, &practice_id);
    let p2 = &vm2.practice;

    assert_eq!(p2.title, "Round-trip song");
    assert_eq!(p2.timeline_duration_ms, expected_timeline);

    // Accompaniment.
    let a2 = p2
        .accompaniment
        .as_ref()
        .expect("the accompaniment reloads");
    assert_eq!(a2.display_name, accomp.display_name);
    assert_eq!(a2.clips.len(), 1);
    assert_eq!(
        a2.primary_clip().unwrap().waveform_peaks.len(),
        expected_accomp_peaks
    );
    assert_eq!(a2.notes.len(), 1);
    assert_eq!(a2.notes[0].content, "count-in here");
    // The accompaniment clip resolves to a readable pooled file.
    let accomp_file = &a2.clips[0].audio_file_path;
    assert!(!accomp_file.starts_with("asset://"));
    assert!(Path::new(accomp_file).exists());

    // Active interpretation + its single take.
    assert_eq!(
        p2.active_version().map(|v| v.id.as_str()),
        Some(version_id.as_str())
    );
    let takes2 = p2.active_vocal_tracks();
    assert_eq!(takes2.len(), 1);
    let take2 = &takes2[0];
    assert_eq!(take2.id, take_id);
    assert_eq!(
        take2.primary_clip().unwrap().waveform_peaks.len(),
        expected_take_peaks
    );

    // Notes round-trip on the take.
    assert_eq!(take2.notes.len(), 1);
    let note2 = &take2.notes[0];
    assert_eq!(note2.content, "sustain this phrase");
    assert_eq!(note2.position_ms, 400);
    assert_eq!(note2.end_ms, Some(900));

    // Technique markers round-trip (sorted by position).
    let mut markers = take2.technique_markers.clone();
    markers.sort_by_key(|m| m.position_ms);
    assert_eq!(markers.len(), 2);
    assert_eq!(markers[0].technique_name, "Twang");
    assert_eq!(markers[0].position_ms, 500);
    assert_eq!(markers[0].end_ms, None, "point marker");
    assert_eq!(markers[1].technique_name, "Cry");
    assert_eq!(markers[1].position_ms, 600);
    assert_eq!(markers[1].end_ms, Some(1200), "range marker preserved");

    // FX round-trips on the take.
    let fx2 = take2.fx.as_ref().expect("committed FX must persist");
    assert!(fx2.eq_on);
    assert!(fx2.comp_on);
    assert!((fx2.send_db - -6.0).abs() < 1e-6);
    assert!((fx2.eq[2].gain_db - 4.5).abs() < 1e-6);

    // Reverb round-trips (the mixer shows the default when none is set).
    let r2 = p2.reverb.unwrap_or(ReverbSettings::DEFAULT);
    assert!((r2.room_size - 0.8).abs() < 1e-6);
    assert!((r2.damping - 0.3).abs() < 1e-6);
    assert!((r2.wet - 0.42).abs() < 1e-6);
    assert!((r2.width - 0.9).abs() < 1e-6);

    // Master volume + lyrics + interpretations.
    assert!((p2.master_volume - 0.66).abs() < 1e-6);
    assert_eq!(
        p2.lyrics
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>(),
        ["first line", "second line", "third line"]
    );
    assert_eq!(p2.versions.len(), 2);
    assert!(p2.versions.iter().any(|v| v.name == "Powerful"));
}

#[test]
fn a_multi_clip_take_round_trips_clip_count_bytes_peaks_and_ordering() {
    let store = Store::new("vocara_multiclip");
    let tmp = store.tmp.path();
    let repo = store.persistence.song_repo();

    let base = repo.create_practice("Multi").unwrap();
    assert_eq!(base.versions.len(), 1);
    let version_id = base.versions[0].id.clone();

    let clip = |name: &str, bytes: &[u8], start_ms: i64, peaks: &[f64]| {
        let f = tmp.join(name);
        fs::write(&f, bytes).unwrap();
        AudioClip {
            id: new_id(),
            audio_file_path: f.to_string_lossy().into_owned(),
            duration_ms: 5000,
            start_ms,
            waveform_peaks: peaks.to_vec(),
        }
    };

    // A vocal lane with TWO clips on the timeline (a punch-in style take).
    // Peaks are float32-exact (powers-of-two fractions) so the content-hash
    // waveform cache round-trips them without precision loss.
    let c0 = clip("a.wav", &[1, 2, 3], 0, &[0.5, 0.25]);
    let c1 = clip("b.wav", &[4, 5, 6, 7], 6000, &[0.75, 0.5, 0.125]);
    let multi = AudioTrack {
        version_id: Some(version_id.clone()),
        clips: vec![c0.clone(), c1.clone()],
        ..AudioTrack::new(
            new_id(),
            AudioTrackKind::Vocal,
            "Two clips",
            DateTime::local_ymd(2026, 1, 1),
        )
    };

    let saved = repo.save_practice(&SongPractice {
        vocal_tracks: vec![Arc::new(multi)],
        versions: base.versions.clone(),
        active_version_id: Some(version_id),
        ..SongPractice::new(
            base.id.clone(),
            base.title.clone(),
            base.created_at,
            base.updated_at,
        )
    });
    assert!(saved.is_ok(), "{saved:?}");

    let loaded = repo.get_practice(&base.id).unwrap();
    assert_eq!(loaded.vocal_tracks.len(), 1);
    let lane = &loaded.vocal_tracks[0];
    assert_eq!(lane.clips.len(), 2, "both clips persist");
    // Ordering is preserved (orderIndex), so clip[0] is the earlier one.
    assert_eq!(lane.clips[0].id, c0.id);
    assert_eq!(lane.clips[1].id, c1.id);
    assert_eq!(lane.clips[1].start_ms, 6000);
    // Bytes resolve back from the content-addressed pool.
    assert_eq!(fs::read(&lane.clips[0].audio_file_path).unwrap(), [1, 2, 3]);
    assert_eq!(
        fs::read(&lane.clips[1].audio_file_path).unwrap(),
        [4, 5, 6, 7]
    );
    // Per-clip waveform peaks survive (cached by content hash).
    assert_eq!(lane.clips[0].waveform_peaks, [0.5, 0.25]);
    assert_eq!(lane.clips[1].waveform_peaks, [0.75, 0.5, 0.125]);
}

#[test]
fn content_addressed_assets_dedup_identical_bytes_and_the_sweep_frees_only_unreferenced_bytes() {
    let store = Store::new("vocara_assets");
    let tmp = store.tmp.path();
    let persistence = &store.persistence;
    let assets = persistence.assets();
    let repo = persistence.song_repo();

    // Importing identical bytes twice yields ONE live asset (deduped by
    // hash).
    let src_a = tmp.join("a.wav");
    let src_a_dup = tmp.join("a-copy.wav");
    fs::write(&src_a, [10, 20, 30, 40]).unwrap();
    fs::write(&src_a_dup, [10, 20, 30, 40]).unwrap(); // same content
    let id_a = assets
        .import_file(&src_a.to_string_lossy(), AssetStore::DEFAULT_KIND)
        .unwrap();
    let id_a_dup = assets
        .import_file(&src_a_dup.to_string_lossy(), AssetStore::DEFAULT_KIND)
        .unwrap();
    assert_eq!(id_a_dup, id_a, "identical bytes reuse the same asset id");

    let live_after_dedup: Vec<AssetRow> =
        persistence.db().select("deleted_at IS NULL", []).unwrap();
    assert_eq!(
        live_after_dedup.len(),
        1,
        "one asset row for one content hash"
    );

    // Wire that asset into a saved take, plus a standalone orphan asset.
    let base = repo.create_practice("GC").unwrap();
    let version_id = base.versions[0].id.clone();
    let kept_path = assets.resolve(&id_a).unwrap().unwrap();
    let vocal = AudioTrack {
        version_id: Some(version_id.clone()),
        clips: vec![AudioClip {
            id: new_id(),
            audio_file_path: kept_path.clone(),
            duration_ms: 1000,
            start_ms: 0,
            waveform_peaks: Vec::new(),
        }],
        ..AudioTrack::new(
            new_id(),
            AudioTrackKind::Vocal,
            "Keep me",
            DateTime::local_ymd(2026, 1, 1),
        )
    };
    let bare = SongPractice {
        versions: base.versions.clone(),
        active_version_id: Some(version_id),
        ..SongPractice::new(
            base.id.clone(),
            base.title.clone(),
            base.created_at,
            base.updated_at,
        )
    };
    repo.save_practice(&SongPractice {
        vocal_tracks: vec![Arc::new(vocal)],
        ..bare.clone()
    })
    .unwrap();

    let orphan = tmp.join("orphan.wav");
    fs::write(&orphan, [99, 98, 97]).unwrap();
    let orphan_id = assets
        .import_file(&orphan.to_string_lossy(), AssetStore::DEFAULT_KIND)
        .unwrap();
    let orphan_path = assets.resolve(&orphan_id).unwrap().unwrap();
    assert!(Path::new(&orphan_path).exists());

    // Sweep reclaims the orphan only; the referenced asset survives.
    // min_age zero because everything here was created milliseconds ago —
    // the production default holds a young unreferenced row back, on the
    // grounds that it is far more likely an in-flight save than garbage.
    assert_eq!(assets.sweep_unreferenced(Duration::ZERO).unwrap(), 1);
    assert_eq!(assets.resolve(&orphan_id).unwrap(), None);
    assert!(!Path::new(&orphan_path).exists());
    assert!(
        assets.resolve(&id_a).unwrap().is_some(),
        "still-referenced asset survives"
    );
    assert!(Path::new(&kept_path).exists());

    // Drop the take (save without it) -> its asset is now unreferenced -> a
    // later sweep frees its bytes too.
    repo.save_practice(&bare).unwrap();
    assert_eq!(assets.sweep_unreferenced(Duration::ZERO).unwrap(), 1);
    assert_eq!(assets.resolve(&id_a).unwrap(), None);
    assert!(!Path::new(&kept_path).exists());
}

#[test]
fn interpretation_switching_keeps_per_version_takes_separate_across_reload() {
    let store = Store::new("vocara_versions");
    let tmp = store.tmp.path();
    let repo = store.persistence.song_repo();

    let base = repo.create_practice("Versions").unwrap();
    let v1 = base.versions[0].clone();
    let v2 = TakeVersion::new(
        new_id(),
        "Interpretation 2",
        DateTime::local_ymd(2026, 1, 1),
    );

    let clip = |name: &str, bytes: &[u8]| {
        let f = tmp.join(name);
        fs::write(&f, bytes).unwrap();
        AudioClip {
            id: new_id(),
            audio_file_path: f.to_string_lossy().into_owned(),
            duration_ms: 1000,
            start_ms: 0,
            waveform_peaks: Vec::new(),
        }
    };
    let d2026 = DateTime::local_ymd(2026, 1, 1);
    let take = |name: &str, version: &str, clip: AudioClip| AudioTrack {
        version_id: Some(version.to_string()),
        clips: vec![clip],
        ..AudioTrack::new(new_id(), AudioTrackKind::Vocal, name, d2026)
    };

    let take_v1 = AudioTrack {
        notes: vec![PracticeNote {
            id: new_id(),
            position_ms: 100,
            end_ms: None,
            content: "v1 note".to_string(),
            created_at: d2026,
            updated_at: d2026,
        }],
        ..take("V1 take", &v1.id, clip("v1.wav", &[1, 1, 1]))
    };
    let take_v2a = take("V2 take A", &v2.id, clip("v2a.wav", &[2, 2, 2]));
    let take_v2b = take("V2 take B", &v2.id, clip("v2b.wav", &[3, 3, 3]));

    repo.save_practice(&SongPractice {
        vocal_tracks: vec![Arc::new(take_v1), Arc::new(take_v2a), Arc::new(take_v2b)],
        versions: vec![v1.clone(), Arc::new(v2.clone())],
        active_version_id: Some(v2.id.clone()),
        ..SongPractice::new(
            base.id.clone(),
            base.title.clone(),
            base.created_at,
            base.updated_at,
        )
    })
    .unwrap();

    let loaded = repo.get_practice(&base.id).unwrap();
    assert_eq!(loaded.versions.len(), 2);
    assert_eq!(loaded.active_version_id.as_deref(), Some(v2.id.as_str()));
    // All three takes persist...
    assert_eq!(loaded.vocal_tracks.len(), 3);
    // ...but each interpretation only sees its own.
    let v2_takes = loaded.active_vocal_tracks();
    assert_eq!(v2_takes.len(), 2);
    let names: Vec<&str> = v2_takes.iter().map(|t| t.display_name.as_str()).collect();
    assert!(names.contains(&"V2 take A") && names.contains(&"V2 take B"));
    assert!(v2_takes
        .iter()
        .all(|t| t.version_id.as_deref() == Some(v2.id.as_str())));

    // The v1 take (and its note) is still there, tagged to v1 — invisible to
    // the active v2 view but recoverable by switching.
    let v1_takes: Vec<_> = loaded
        .vocal_tracks
        .iter()
        .filter(|t| t.version_id.as_deref() == Some(v1.id.as_str()))
        .collect();
    assert_eq!(v1_takes.len(), 1);
    assert_eq!(v1_takes[0].display_name, "V1 take");
    assert_eq!(v1_takes[0].notes.len(), 1);
    assert_eq!(v1_takes[0].notes[0].content, "v1 note");
}
