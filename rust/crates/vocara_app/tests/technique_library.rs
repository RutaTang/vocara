//! Port of `client/integration_test/technique_library_test.dart`: the
//! technique library end to end over a real (in-memory) store and the real
//! media service — create and filter through the library view model, stage,
//! analyze, caption, save and reload a reference example through the detail
//! view model, discard a staged one, the media analysis on a clean and an
//! undecodable file, and a technique marker that outlives its library entry.
//!
//! The view models hand their I/O back as jobs (see their module docs); a
//! small rig here runs each on the test thread and lands it, as the screens
//! do on a worker.
//!
//! The analysis decodes through the engine bridge, which exists only with
//! the `device` feature (the macOS CI run, as Dart's integration test ran on
//! the Mac). A build without it has no engine to decode with, so there the
//! same files go through the same analysis service on the WAV reader's
//! decode instead (every fixture is a 44.1 kHz mono WAV, the rate the engine
//! decodes analysis at).

use std::collections::VecDeque;
use std::f64::consts::PI;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use vocara_app::ui::song_practice_detail::view_model::analysis_source::AnalysisSource;
use vocara_app::ui::technique_library::view_model::technique_detail_view_model::{
    Answer, Effect as DetailEffect, Job, Next, TechniqueDetailDeps, TechniqueDetailViewModel,
};
use vocara_app::ui::technique_library::view_model::technique_library_view_model::{
    Effect as LibraryEffect, TechniqueLibraryViewModel,
};
use vocara_model::models::audio_track::{AudioTrack, AudioTrackKind};
use vocara_model::models::technique::Technique;
use vocara_model::models::technique_analysis::TechniqueAnalysis;
use vocara_model::models::technique_marker::TechniqueMarker;
use vocara_model::models::technique_media::TechniqueMediaKind;
use vocara_model::util::dart_date_time::DateTime;
use vocara_model::util::id::new_id;
use vocara_services::audio_exporter::AudioExporter;
use vocara_services::audio_file_service::AudioFileService;
use vocara_services::local_storage_service::LocalStorageService;
use vocara_services::media_analysis_service::{MediaAnalyses, MediaAnalysisService};
use vocara_services::persistence::Persistence;
use vocara_services::recording_finalizer::RecordingFinalizer;
use vocara_services::technique_media_service::TechniqueMediaService;
use vocara_store::repositories::{SongPracticeRepository, TechniqueRepository};

/// Writes a mono 16-bit PCM WAV at [freq] Hz and returns its path. A real,
/// decodable tone so the media analysis (which decodes via the native
/// engine) produces a non-empty analysis.
fn write_sine_wav(path: &Path, seconds: u32, freq: f64) -> PathBuf {
    const SAMPLE_RATE: u32 = 44100;
    let samples = SAMPLE_RATE * seconds;
    let data_size = samples * 2;
    let mut bytes = Vec::with_capacity(44 + data_size as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_size).to_le_bytes());
    bytes.extend_from_slice(b"WAVE");
    bytes.extend_from_slice(b"fmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
    bytes.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    bytes.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes()); // byte rate
    bytes.extend_from_slice(&2u16.to_le_bytes()); // block align
    bytes.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_size.to_le_bytes());
    for i in 0..samples {
        let value =
            ((2.0 * PI * freq * f64::from(i) / f64::from(SAMPLE_RATE)).sin() * 0.5 * 32767.0)
                .round() as i16;
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
    path.to_path_buf()
}

/// The media analysis as the app builds it: the engine bridge's decode.
#[cfg(feature = "device")]
fn media_analysis() -> MediaAnalysisService {
    MediaAnalysisService::new()
}

/// No engine in this build (see the module doc): the analysis service over
/// the WAV reader's decode of the same file.
#[cfg(not(feature = "device"))]
fn media_analysis() -> MediaAnalysisService {
    use vocara_services::audio_dsp::read_wav_mono;
    use vocara_services::engine::media::MediaError;
    use vocara_services::engine::Pcm;
    MediaAnalysisService::with_decoder(|path, _cap| {
        let bytes = std::fs::read(path).map_err(|_| MediaError::Read)?;
        Ok(read_wav_mono(&bytes)
            .map(|pcm| Pcm::mono(pcm.samples, u32::try_from(pcm.sample_rate).unwrap_or(0))))
    })
}

/// Dart's `setUp`: an in-memory store, the media service over a scratch
/// root, and a finalizer of the test's own (the tests run in parallel).
struct World {
    persistence: Persistence,
    media: Arc<TechniqueMediaService>,
    finalizer: Arc<RecordingFinalizer>,
    scratch: tempfile::TempDir,
}

impl World {
    fn new() -> World {
        let scratch = tempfile::tempdir().unwrap();
        let storage = Arc::new(LocalStorageService::with_root(scratch.path().join("app")));
        World {
            persistence: Persistence::memory().unwrap(),
            media: Arc::new(TechniqueMediaService::new(storage)),
            finalizer: Arc::new(RecordingFinalizer::for_testing()),
            scratch,
        }
    }

    fn tech_repo(&self) -> Arc<dyn TechniqueRepository> {
        self.persistence.tech_repo().clone()
    }

    fn song_repo(&self) -> Arc<dyn SongPracticeRepository> {
        self.persistence.song_repo().clone()
    }

    /// A WAV tone under the scratch dir (Dart's temporary directory).
    fn tone(&self, name: &str, freq: f64) -> PathBuf {
        write_sine_wav(&self.scratch.path().join(name), 1, freq)
    }

    fn library(&self) -> Library {
        let (vm, load) = TechniqueLibraryViewModel::new(self.tech_repo(), self.media.clone());
        let mut lib = Library { vm };
        lib.run(load);
        lib
    }

    /// A detail view model over [technique_id], its first load landed.
    fn detail(&self, technique_id: &str) -> Detail {
        let exporter = AudioExporter::new(
            Arc::new(AudioFileService::new(Arc::new(
                LocalStorageService::with_root(self.scratch.path().join("app")),
            ))),
            |_| Ok(None),
        )
        .with_finalizer(Arc::clone(&self.finalizer));
        let deps = TechniqueDetailDeps {
            analysis: Arc::new(media_analysis()) as Arc<dyn MediaAnalyses>,
            finalizer: Arc::clone(&self.finalizer),
            ..TechniqueDetailDeps::new(
                technique_id,
                self.tech_repo(),
                self.media.clone(),
                Arc::new(exporter),
            )
        };
        let (vm, next) = TechniqueDetailViewModel::new(deps);
        let mut d = Detail {
            vm,
            queue: VecDeque::new(),
            answers: Vec::new(),
        };
        d.apply(next);
        d.settle();
        d
    }

    /// Creates [name] through the library view model and returns it.
    fn create_technique(&self, name: &str) -> Technique {
        let mut lib = self.library();
        let created = lib.create(name);
        lib.vm.dispose();
        created
    }
}

impl Drop for World {
    fn drop(&mut self) {
        // The in-memory store's asset pool is a temp dir of its own.
        let _ = std::fs::remove_dir_all(self.persistence.assets().root());
    }
}

/// The library view model, its commands run on the test thread.
struct Library {
    vm: TechniqueLibraryViewModel,
}

impl Library {
    fn run(&mut self, effect: Option<LibraryEffect>) {
        let mut queue: VecDeque<LibraryEffect> = effect.into_iter().collect();
        while let Some(e) = queue.pop_front() {
            let (done, cleanup) = e.run();
            queue.extend(self.vm.land(done));
            if let Some(cleanup) = cleanup {
                queue.extend(self.vm.land(cleanup.run()));
            }
        }
    }

    /// `createTechnique.execute(name)`, settled; the technique it created.
    fn create(&mut self, name: &str) -> Technique {
        let effect = self.vm.execute_create(name);
        self.run(effect);
        let command = self.vm.create_technique();
        assert!(!command.running());
        assert!(
            command.completed(),
            "creating a named technique should succeed"
        );
        command.result().unwrap().clone().unwrap()
    }
}

/// The detail view model, its jobs run on the test thread in the order
/// queued.
struct Detail {
    vm: TechniqueDetailViewModel,
    queue: VecDeque<Job>,
    answers: Vec<Answer>,
}

impl Detail {
    fn apply(&mut self, next: Next) {
        for effect in next.effects {
            if let DetailEffect::Run(job) = effect {
                self.queue.push_back(job);
            }
        }
        self.answers.extend(next.answers);
    }

    fn settle(&mut self) {
        while let Some(job) = self.queue.pop_front() {
            let done = job.run();
            let next = self.vm.land(done);
            self.apply(next);
        }
    }

    /// A call, then everything it started runs and lands.
    fn call(&mut self, f: impl FnOnce(&mut TechniqueDetailViewModel) -> Next) {
        let next = f(&mut self.vm);
        self.apply(next);
        self.settle();
    }

    /// The answer the last call settled with.
    fn take_answers(&mut self) -> Vec<Answer> {
        std::mem::take(&mut self.answers)
    }

    /// `dispose`: what the page hands over runs detached, as the screen's.
    fn dispose(mut self) {
        let next = self.vm.dispose();
        for effect in next.effects {
            if let DetailEffect::Run(job) = effect {
                job.detach();
            }
        }
        for job in self.queue.drain(..) {
            job.detach();
        }
    }
}

#[test]
fn library_create_technique_appears_in_the_list_and_filters_by_query() {
    let world = World::new();
    let mut lib = world.library();

    // The load command runs on construction; it has finished.
    assert!(!lib.vm.load().running());
    assert!(lib.vm.all().is_empty());

    let created = lib.create("Twang");
    assert_eq!(created.name, "Twang");
    assert!(lib.vm.all().iter().any(|t| t.id == created.id));
    assert!(lib.vm.filtered().iter().any(|t| t.id == created.id));

    // A non-matching free-text query hides it; a matching one keeps it.
    lib.vm.set_query("zzz-no-match");
    assert!(lib.vm.filtered().is_empty());
    lib.vm.set_query("twa");
    assert!(lib.vm.filtered().iter().any(|t| t.id == created.id));
    lib.vm.set_query("");

    lib.vm.dispose();
}

#[test]
fn detail_reference_media_staged_saved_and_reloaded_round_trips_with_a_real_analysis() {
    let world = World::new();

    // 1) Create a technique through the library VM.
    let technique_id = world.create_technique("Belting").id;

    // 2) Open its detail VM.
    let wav_path = world.tone("ref.wav", 440.0);
    let mut detail = world.detail(&technique_id);
    assert!(detail.vm.technique().is_some());
    assert!(detail.vm.effective_media().is_empty());

    // 3) Stage a reference media item in the draft (not yet persisted).
    detail.vm.begin_media_draft();
    detail.call(|vm| vm.add_media(vec![wav_path.to_string_lossy().into_owned()]));
    assert!(!detail.vm.importing_media());

    assert!(
        detail.vm.has_media_draft(),
        "a staged add should mark the draft dirty"
    );
    assert_eq!(
        detail.vm.effective_media().len(),
        1,
        "the staged example shows in the draft view"
    );
    assert_eq!(detail.vm.audio_examples().len(), 1);
    // Still nothing persisted on the underlying technique.
    assert!(detail.vm.technique().unwrap().media.is_empty());

    let staged_path = detail.vm.reference_media().unwrap().path;
    assert!(
        Path::new(&staged_path).exists(),
        "add_media copies the file into the sandbox immediately"
    );

    // 4) Run the ground-truth (reference) analysis on the staged example.
    detail.call(|vm| vm.ensure_reference_analysis());
    assert!(!detail.vm.reference_analysis_loading());

    // A real 440 Hz tone decodes; the analysis should be present and sane.
    assert!(
        !detail.vm.reference_analysis_failed(),
        "a clean 440 Hz tone should decode and analyze ({:?})",
        detail.vm.reference_analysis_failure_message()
    );
    let ref_analysis = detail.vm.reference_analysis().expect("an analysis");
    assert!(
        ref_analysis.duration_ms > 500,
        "a ~1s tone reports a real duration"
    );
    assert!(ref_analysis.metrics.frames > 0);
    // The analysis subject getters (AnalysisSource) should be wired.
    assert!(detail.vm.selected_metrics().is_some());
    assert!(detail.vm.selected_spectrogram().is_some());
    let track = detail.vm.selected_track().expect("a subject track");
    assert_eq!(track.duration_ms(), ref_analysis.duration_ms);

    // 5) Caption the staged example, then SAVE the draft.
    let media_id = detail.vm.effective_media()[0].id.clone();
    detail.vm.rename_media(&media_id, "Demo take");
    let edited = Technique {
        tags: vec!["EVT".into(), "Quality".into()],
        summary: "bright, ringing".into(),
        cues: "narrow the AES".into(),
        updated_at: DateTime::now(),
        ..detail.vm.technique().unwrap().clone()
    };
    detail.take_answers();
    detail.call(|vm| vm.save(edited));
    assert_eq!(
        detail.take_answers(),
        vec![Answer::Saved(true)],
        "save should persist"
    );
    // After save the draft is committed/cleared.
    assert!(!detail.vm.has_media_draft());

    detail.dispose();

    // 6) Fresh reload from the SAME store and assert the round-trip.
    let reloaded = world.tech_repo().get_technique(&technique_id).unwrap();
    assert_eq!(reloaded.name, "Belting");
    assert_eq!(reloaded.tags, vec!["EVT", "Quality"]);
    assert_eq!(reloaded.summary, "bright, ringing");
    assert_eq!(reloaded.cues, "narrow the AES");
    assert_eq!(
        reloaded.media.len(),
        1,
        "the saved reference example survives reload"
    );
    let reloaded_media = &reloaded.media[0];
    assert_eq!(
        reloaded_media.label, "Demo take",
        "the caption snapshot persists"
    );
    assert_eq!(reloaded_media.kind, TechniqueMediaKind::Audio);
    // The pooled file is readable after reload.
    let pooled = std::fs::metadata(&reloaded_media.path).expect("the pooled file");
    assert!(pooled.len() > 0);

    // 7) Re-open the detail VM on the reloaded technique and confirm the
    //    ground-truth analysis recomputes from the pooled media.
    let mut detail2 = world.detail(&technique_id);
    assert_eq!(detail2.vm.audio_examples().len(), 1);
    detail2.call(|vm| vm.ensure_reference_analysis());
    assert!(!detail2.vm.reference_analysis_loading());
    assert!(!detail2.vm.reference_analysis_failed());
    assert!(detail2.vm.reference_analysis().is_some());
    detail2.dispose();
}

#[test]
fn detail_a_staged_media_draft_can_be_discarded_without_persisting() {
    let world = World::new();
    let technique_id = world.create_technique("Curbing").id;

    let wav_path = world.tone("drop.wav", 440.0);
    let mut detail = world.detail(&technique_id);

    detail.vm.begin_media_draft();
    detail.call(|vm| vm.add_media(vec![wav_path.to_string_lossy().into_owned()]));
    assert_eq!(detail.vm.effective_media().len(), 1);
    let staged_path = detail.vm.effective_media()[0].path.clone();
    assert!(Path::new(&staged_path).exists());

    // Discard the draft: the staged example and its copied file are dropped.
    detail.take_answers();
    detail.call(|vm| vm.discard_media_draft());
    assert_eq!(detail.take_answers(), vec![Answer::DraftDiscarded(true)]);
    assert!(!detail.vm.has_media_draft());
    assert!(detail.vm.effective_media().is_empty());
    // The orphaned sandbox copy is deleted.
    assert!(
        !Path::new(&staged_path).exists(),
        "a discarded staged file should be removed from the sandbox"
    );

    detail.dispose();

    // Nothing persisted.
    let reloaded = world.tech_repo().get_technique(&technique_id).unwrap();
    assert!(reloaded.media.is_empty());
}

#[test]
fn analysis_service_a_clean_tone_analyzes_and_an_undecodable_file_is_none() {
    let world = World::new();
    let analysis = media_analysis();

    let tone_path = world.tone("tone.wav", 220.0);
    let tone: Option<TechniqueAnalysis> = analysis
        .analyze(&tone_path.to_string_lossy(), None)
        .unwrap();
    let tone = tone.expect("a valid WAV tone should analyze");
    assert!(tone.duration_ms > 500);
    assert!(tone.metrics.frames > 0);
    assert!(tone.spectrogram.frames > 0);
    // No f0 value is infinite: a voiced frame carries a finite pitch and an
    // unvoiced one NaN (the documented marker), never ±Infinity.
    for &f in &tone.metrics.f0 {
        assert!(f.is_finite() || f.is_nan());
    }

    // A file that is not decodable audio yields none, not a failure.
    let bogus = world.scratch.path().join("bogus.wav");
    std::fs::write(&bogus, [7u8; 64]).unwrap();
    let bad = analysis.analyze(&bogus.to_string_lossy(), None);
    assert!(
        matches!(bad, Ok(None)),
        "an undecodable file returns none rather than an error: {:?}",
        bad.map(|a| a.is_some())
    );
}

#[test]
fn a_technique_marker_keeps_its_name_snapshot_after_the_library_entry_is_deleted() {
    // A logical ref, no hard FK.
    let world = World::new();

    // 1) Create a library technique to reference.
    let technique = world.create_technique("Vibrato");

    // 2) Create a song practice and pin a technique marker on a vocal take
    //    that references the library technique (with a name snapshot). An
    //    empty-clip vocal lane needs no audio asset but still carries markers.
    let practice = world.song_repo().create_practice("Marker song").unwrap();
    let now = DateTime::now();
    let marker = TechniqueMarker {
        id: new_id(),
        position_ms: 1500,
        end_ms: None,
        technique_id: technique.id.clone(),
        technique_name: technique.name.clone(), // snapshot at creation time
        created_at: now,
        updated_at: now,
    };
    let take = AudioTrack {
        version_id: practice.active_version_id.clone(),
        technique_markers: vec![marker],
        ..AudioTrack::new(new_id(), AudioTrackKind::Vocal, "Take 1", now)
    };
    let take_id = take.id.clone();
    let with_marker = vocara_model::models::song_practice::SongPractice {
        vocal_tracks: vec![Arc::new(take)],
        ..practice.clone()
    };
    let saved = world.song_repo().save_practice(&with_marker).unwrap();

    // The marker round-trips on the saved practice.
    let saved_take = saved.vocal_tracks.iter().find(|t| t.id == take_id).unwrap();
    assert_eq!(saved_take.technique_markers.len(), 1);
    assert_eq!(saved_take.technique_markers[0].technique_id, technique.id);
    assert_eq!(saved_take.technique_markers[0].technique_name, "Vibrato");

    // 3) DELETE the library technique. The marker's ref now dangles by design.
    world.tech_repo().delete_technique(&technique.id).unwrap();
    assert!(world.tech_repo().get_technique(&technique.id).is_err());

    // 4) Reload the practice from the SAME store: the marker survives with
    //    its name snapshot intact even though the library entry is gone.
    let reloaded = world.song_repo().get_practice(&practice.id).unwrap();
    let reloaded_take = reloaded
        .vocal_tracks
        .iter()
        .find(|t| t.id == take_id)
        .unwrap();
    assert_eq!(
        reloaded_take.technique_markers.len(),
        1,
        "the marker is not cascade-deleted with the technique"
    );
    let reloaded_marker = &reloaded_take.technique_markers[0];
    assert_eq!(
        reloaded_marker.technique_id, technique.id,
        "the logical ref is preserved (it may now dangle)"
    );
    assert_eq!(
        reloaded_marker.technique_name, "Vibrato",
        "the name snapshot keeps the marker readable"
    );
    assert_eq!(reloaded_marker.position_ms, 1500);
}
