//! Ports of the AudioFileService parts of `client/test/import_cap_test.dart`
//! and `client/test/recording_recovery_test.dart` (placement, aligned start,
//! the WAV probe's contract).
#![cfg(unix)]

use std::fs;
use std::io;
use std::path::Path;
use std::sync::Arc;

use vocara_model::util::id::new_id;
use vocara_services::audio_file_service::*;
use vocara_services::local_storage_service::LocalStorageService;

const PRACTICE: &str = "P0000000000000000000000000";

fn service(root: &Path) -> AudioFileService {
    AudioFileService::new(Arc::new(LocalStorageService::with_root(root)))
}

/// A sparse file whose LENGTH exceeds the import cap without the disk cost.
fn oversized(dir: &Path) -> String {
    let f = dir.join("huge.wav");
    let file = fs::File::create(&f).unwrap();
    file.set_len(MAX_IMPORT_BYTES + 1).unwrap();
    f.to_string_lossy().into_owned()
}

fn format_message(r: Result<String>) -> String {
    match r {
        Err(AudioFileError::Format(m)) => m,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn import_refuses_an_oversized_source_from_a_stat_before_any_bytes_are_copied() {
    let tmp = tempfile::tempdir().unwrap();
    let src = oversized(tmp.path());
    let m = format_message(service(tmp.path()).import_audio(PRACTICE, &src));
    assert!(m.contains("too large"), "{m}");
    // Nothing copied: the practice's audio dir was never even created.
    assert!(
        !tmp.path().join("audio").exists(),
        "the refusal must precede every side effect"
    );
}

#[test]
fn import_refuses_a_pipe_and_an_empty_source_without_waiting_on_either() {
    let tmp = tempfile::tempdir().unwrap();
    let fifo = tmp.path().join("take.wav");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(made.success());
    let empty = tmp.path().join("empty.wav");
    fs::write(&empty, []).unwrap();
    let files = service(tmp.path());
    assert_eq!(
        format_message(files.import_audio(PRACTICE, &fifo.to_string_lossy())),
        "The file couldn't be read."
    );
    assert_eq!(
        format_message(files.import_audio(PRACTICE, &empty.to_string_lossy())),
        "This file is empty."
    );
    assert!(!tmp.path().join("audio").exists());
}

#[test]
fn import_refuses_an_id_that_is_not_a_path_component() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("a.wav");
    fs::write(&src, [1, 2, 3]).unwrap();
    assert!(matches!(
        service(tmp.path()).import_audio("../etc", &src.to_string_lossy()),
        Err(AudioFileError::InvalidId(_))
    ));
}

#[test]
fn copy_bounded_cuts_off_a_source_that_outgrows_the_cap_and_leaves_no_litter() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("growing.bin");
    fs::write(&src, vec![1u8; 64 * 1024]).unwrap();
    let dest = tmp.path().join("dest.bin");
    let r = copy_bounded(&src.to_string_lossy(), &dest.to_string_lossy(), 16 * 1024);
    assert!(matches!(r, Err(AudioFileError::Format(_))));
    assert!(!dest.exists(), "no half file");
    let litter: Vec<_> = fs::read_dir(tmp.path())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("dest.bin"))
        .collect();
    assert!(litter.is_empty(), "no temp litter");
}

#[test]
fn a_normal_sized_source_still_imports() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("small.wav");
    fs::write(&src, [1, 2, 3, 4]).unwrap();
    let dest = service(tmp.path())
        .import_audio(PRACTICE, &src.to_string_lossy())
        .unwrap();
    assert_eq!(fs::read(&dest).unwrap(), [1, 2, 3, 4]);
    assert!(dest.contains(&format!("audio/{PRACTICE}/imports/")));
    assert!(dest.ends_with(".wav"));
}

#[test]
fn a_recording_name_carries_its_lane_and_start_and_others_carry_none() {
    assert_eq!(
        recording_placement_of("/a/p1/01ABC~01HZZZZZZZZZZZZZZZZZZZZZZZ~1500.wav"),
        Some(RecordingPlacement {
            take_id: "01HZZZZZZZZZZZZZZZZZZZZZZZ".into(),
            start_ms: 1500
        })
    );
    for other in [
        "/a/p1/01ABC.wav",
        "/a/p1/01ABC~01HZZZZZZZZZZZZZZZZZZZZZZZ.wav",
        "/a/p1/01ABC~~1500.wav",
        "/a/p1/01ABC~TAKE9~1500.wav", // not an id of the minted shape
        "/a/p1/01ABC~01HZZZZZZZZZZZZZZZZZZZZZZZ~-5.wav",
        "/a/p1/01ABC~01HZZZZZZZZZZZZZZZZZZZZZZZ~x.wav",
    ] {
        assert_eq!(recording_placement_of(other), None, "{other}");
    }
}

#[test]
fn new_recording_paths_carry_lane_and_start_only_for_a_minted_id() {
    let tmp = tempfile::tempdir().unwrap();
    let files = service(tmp.path());
    let take = "01HZZZZZZZZZZZZZZZZZZZZZZZ";
    let p = files
        .new_recording_path(PRACTICE, "wav", Some(take), Some(3000))
        .unwrap();
    assert_eq!(recording_placement_of(&p).unwrap().start_ms, 3000);
    let plain = files
        .new_recording_path(PRACTICE, "wav", Some("TAKE"), Some(3000))
        .unwrap();
    assert_eq!(recording_placement_of(&plain), None);
}

#[test]
fn the_stops_aligned_start_lands_after_the_starts() {
    // The start's write is not waited on; on a slow disk the stop's ran
    // alongside it and the file came out "-512" or "0512", never "0".
    let tmp = tempfile::tempdir().unwrap();
    let staged = tmp
        .path()
        .join(format!("{}~01HZZZZZZZZZZZZZZZZZZZZZZZ~3000.wav", new_id()))
        .to_string_lossy()
        .into_owned();
    for i in 0..40 {
        let at_start = write_aligned_start_to(&staged, -512);
        let at_stop = write_aligned_start_to(&staged, 0);
        assert!(at_start.wait() && at_stop.wait());
        assert_eq!(
            fs::read_to_string(format!("{staged}.place")).unwrap(),
            "0",
            "run {i}"
        );
    }
    assert_eq!(recording_placement_of(&staged).unwrap().start_ms, 0);
    assert!(!Path::new(&format!("{staged}.place.tmp")).exists());
}

fn mono_wav(frames: usize) -> Vec<u8> {
    let data = (frames * 2) as u32;
    let mut b = Vec::new();
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    for v in [1u16, 1] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    b.extend_from_slice(&48_000u32.to_le_bytes());
    b.extend_from_slice(&96_000u32.to_le_bytes());
    for v in [2u16, 16] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data.to_le_bytes());
    b.resize(44 + frames * 2, 0);
    b
}

struct Faulty<'a> {
    real: &'a Path,
    stat_fails: bool,
    read_fails: bool,
}

impl ProbeSource for Faulty<'_> {
    fn length(&self) -> io::Result<u64> {
        if self.stat_fails {
            return Err(io::Error::other("simulated I/O error"));
        }
        self.real.length()
    }
    fn read_head(&self, n: usize) -> io::Result<Vec<u8>> {
        if self.read_fails {
            return Err(io::Error::other("simulated read error"));
        }
        self.real.read_head(n)
    }
}

#[test]
fn probe_wav_fails_on_an_unreadable_file_instead_of_answering_none() {
    // The contract the recovery classification rests on: None means
    // "provably not the recorder's shape", and only the shape checks may
    // produce it. A stat or a header read that fails is not evidence.
    let tmp = tempfile::tempdir().unwrap();
    let good = tmp.path().join("good.wav");
    fs::write(&good, mono_wav(4800)).unwrap();
    assert_eq!(
        probe_wav(&good).unwrap(),
        Some(WavProbe {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 16,
            data_bytes: 9600
        })
    );
    for (stat_fails, read_fails) in [(true, false), (false, true)] {
        let f = Faulty {
            real: &good,
            stat_fails,
            read_fails,
        };
        assert!(probe_wav_from(&f).is_err());
    }
    // A file that is merely GONE is still a plain None.
    assert_eq!(probe_wav(&tmp.path().join("gone.wav")).unwrap(), None);
}

#[test]
fn probe_wav_answers_none_for_shapes_that_are_not_the_recorders() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("x.wav");
    fs::write(&f, b"short").unwrap();
    assert_eq!(probe_wav(&f).unwrap(), None);
    let mut wav = mono_wav(10);
    wav[20] = 2; // not PCM or float
    fs::write(&f, &wav).unwrap();
    assert_eq!(probe_wav(&f).unwrap(), None);
    // A header claiming more than the file holds is clamped to the file.
    let mut wav = mono_wav(10);
    wav[40..44].copy_from_slice(&1_000_000u32.to_le_bytes());
    fs::write(&f, &wav).unwrap();
    assert_eq!(probe_wav(&f).unwrap().unwrap().data_bytes, 20);
}

#[test]
fn delete_audio_refuses_pool_files_and_asset_refs_and_ignores_missing_files() {
    let tmp = tempfile::tempdir().unwrap();
    let files = service(tmp.path());
    let pooled = tmp.path().join("assets/ab/abcd.wav");
    fs::create_dir_all(pooled.parent().unwrap()).unwrap();
    fs::write(&pooled, b"x").unwrap();
    files.delete_audio(&pooled.to_string_lossy()).unwrap();
    assert!(
        pooled.exists(),
        "a pooled file is the AssetStore's to reclaim"
    );
    files
        .delete_audio("asset://01HZZZZZZZZZZZZZZZZZZZZZZZ")
        .unwrap();
    let mine = tmp.path().join("audio/x.wav");
    fs::create_dir_all(mine.parent().unwrap()).unwrap();
    fs::write(&mine, b"x").unwrap();
    files.delete_audio(&mine.to_string_lossy()).unwrap();
    assert!(!mine.exists());
    files.delete_audio(&mine.to_string_lossy()).unwrap(); // already gone: fine
}
