//! Port of `client/integration_test/audio_pipeline_test.dart`: import →
//! waveform analysis → multi-track playback → a note, through the song
//! screen's view model over a real (in-memory) store and the real file,
//! waveform and spectrogram services.
//!
//! The view model hands its I/O back as effects (see its module doc); the
//! rig here runs each job on the test thread and answers it, as the screen
//! does on a worker.
//!
//! The import transcodes the 44.1 kHz tone to an engine-rate WAV, the
//! waveform decodes through the engine bridge and the transport plays on the
//! native engine, all of which exist only with the `device` feature (the
//! macOS CI run, as Dart's integration test ran on the Mac). A build without
//! it has no engine: there the tone is written at the engine's rate, so the
//! import keeps the file as it is (no transcode is due), its waveform comes
//! from the same waveform analysis over the WAV reader's decode, and the
//! transport drives the engine-less graph service (silent, never failing).

use std::collections::VecDeque;
use std::f64::consts::PI;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use vocara_app::ui::song_practice_detail::view_model::song_practice_detail_view_model::{
    Deps, Effect, Options, Reply, SongPracticeDetailViewModel,
};
use vocara_services::app_settings_service::AppSettingsService;
use vocara_services::audio_file_service::AudioFileService;
use vocara_services::local_storage_service::LocalStorageService;
use vocara_services::persistence::Persistence;
use vocara_services::recording_finalizer::RecordingFinalizer;
use vocara_services::source_separation_service::FakeSourceSeparationService;
use vocara_services::spectrogram_service::SpectrogramService;
use vocara_store::repositories::SongPracticeRepository;

/// The tone's rate: Dart's 44.1 kHz where the engine can transcode it, the
/// engine's own where there is no engine (see the module doc).
#[cfg(feature = "device")]
const SAMPLE_RATE: u32 = 44100;
#[cfg(not(feature = "device"))]
const SAMPLE_RATE: u32 = vocara_services::engine::ENGINE_RATE;

/// Writes a mono 16-bit PCM WAV sine tone and returns its path.
fn write_sine_wav(path: &Path, seconds: u32) -> PathBuf {
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
            ((2.0 * PI * 440.0 * f64::from(i) / f64::from(SAMPLE_RATE)).sin() * 0.5 * 32767.0)
                .round() as i16;
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
    path.to_path_buf()
}

/// The view model's options: the app's defaults (the native engine, the
/// engine's waveform decode), and a quit registry of the test's own.
#[cfg(feature = "device")]
fn options() -> Options {
    Options {
        finalizer: Some(Arc::new(RecordingFinalizer::for_testing())),
        ..Options::default()
    }
}

/// No engine in this build (see the module doc): the waveform analysis over
/// the WAV reader's decode of the imported file.
#[cfg(not(feature = "device"))]
fn options() -> Options {
    use vocara_services::audio_dsp::read_wav_mono;
    use vocara_services::engine::media::MediaError;
    use vocara_services::engine::Pcm;
    use vocara_services::waveform_service::{analyze_with, DEFAULT_TARGET_BUCKETS};
    Options {
        finalizer: Some(Arc::new(RecordingFinalizer::for_testing())),
        analyze: Some(Arc::new(|path: &str| {
            analyze_with(path, DEFAULT_TARGET_BUCKETS, |p| {
                let bytes = std::fs::read(p).map_err(|_| MediaError::Read)?;
                Ok(read_wav_mono(&bytes)
                    .map(|pcm| Pcm::mono(pcm.samples, u32::try_from(pcm.sample_rate).unwrap_or(0))))
            })
            .map_err(|e| e.0)
        })),
        ..Options::default()
    }
}

/// Runs the view model's effects on the test thread, in order, answering
/// each as the screen will.
struct Rig {
    vm: SongPracticeDetailViewModel,
    queue: VecDeque<Effect>,
    toasts: Vec<String>,
    replies: Vec<Reply>,
}

impl Rig {
    fn take(&mut self, effects: Vec<Effect>) {
        self.queue.extend(effects);
    }

    fn settle(&mut self) {
        while let Some(effect) = self.queue.pop_front() {
            let next = match effect {
                Effect::Run(job) => self.vm.job_done(job.run()),
                Effect::Analysis(job) => self.vm.analysis_job_done(job.run()),
                Effect::AnalysisMicPermission => self.vm.analysis_mic_permission_answered(false),
                Effect::MonitorMicPermission => self.vm.monitor_permission_answered(false),
                Effect::LaneHealRetry { .. } => Vec::new(),
                Effect::Toast(message) => {
                    self.toasts.push(message);
                    Vec::new()
                }
                Effect::Reply(reply) => {
                    self.replies.push(reply);
                    Vec::new()
                }
                Effect::Leave => Vec::new(),
                Effect::StartRecording(_)
                | Effect::RecordingWork(_)
                | Effect::FinalizeTake(..)
                | Effect::FinalizeRetry { .. }
                | Effect::InterpretationsCleanUp(..) => {
                    panic!("the pipeline records nothing and edits no interpretation")
                }
            };
            self.queue.extend(next);
        }
    }

    fn call(&mut self, f: impl FnOnce(&mut SongPracticeDetailViewModel) -> Vec<Effect>) {
        let effects = f(&mut self.vm);
        self.take(effects);
        self.settle();
    }
}

#[test]
fn import_waveform_analysis_and_multi_track_playback_pipeline() {
    let tmp = tempfile::tempdir().unwrap();
    let wav_path = write_sine_wav(&tmp.path().join("tone.wav"), 1);

    let storage = Arc::new(LocalStorageService::with_root(tmp.path().join("app")));
    let persistence = Persistence::memory().unwrap();
    let repo = persistence.song_repo().clone();

    let practice_id = repo.create_practice("Pipeline test").unwrap().id;

    // Fire-and-forget load, matching the production wiring (the view model
    // tolerates attach-before-load).
    let app_settings = Arc::new(AppSettingsService::new(storage.clone()));
    app_settings.load();

    let deps = Deps {
        repository: repo.clone() as Arc<dyn SongPracticeRepository>,
        technique_repository: persistence.tech_repo().clone(),
        audio_files: Arc::new(AudioFileService::new(storage.clone())),
        spectrograms: Arc::new(SpectrogramService::new()),
        separation: Arc::new(FakeSourceSeparationService::default()),
        app_settings,
    };
    let (vm, effects) = SongPracticeDetailViewModel::new(deps, practice_id.clone(), options());
    let mut rig = Rig {
        vm,
        queue: VecDeque::new(),
        toasts: Vec::new(),
        replies: Vec::new(),
    };
    rig.take(effects);

    // Let the initial load settle.
    rig.settle();
    assert!(rig.vm.practice().is_some());

    // Import the generated tone as the accompaniment.
    let started = rig.vm.add_accompaniment(&wav_path.to_string_lossy());
    rig.take(started.effects);
    rig.settle();
    assert!(!rig.vm.busy());
    assert!(
        rig.replies.contains(&Reply::Done(started.call)),
        "the import answered its caller"
    );

    let accompaniment = rig
        .vm
        .practice()
        .unwrap()
        .accompaniment
        .clone()
        .unwrap_or_else(|| {
            panic!(
                "accompaniment should be set ({:?}, toasts {:?})",
                rig.vm.error_message(),
                rig.toasts
            )
        });
    assert!(
        !accompaniment
            .primary_clip()
            .expect("a clip")
            .waveform_peaks
            .is_empty(),
        "the waveform analysis should produce peaks"
    );
    assert!(
        accompaniment.duration_ms() > 500,
        "a ~1s tone should report a real duration"
    );
    assert_eq!(rig.vm.error_message(), None, "toasts: {:?}", rig.toasts);

    // Exercise the engine's transport: play, then pause.
    rig.call(|vm| vm.playback_do(|p, host| p.toggle_play(host)));
    std::thread::sleep(std::time::Duration::from_millis(400));
    rig.vm.drain_engine_events();
    rig.call(|vm| vm.playback_do(|p, host| p.toggle_play(host)));
    std::thread::sleep(std::time::Duration::from_millis(100));
    rig.vm.drain_engine_events();

    // Pin a note at the playhead on the accompaniment track.
    rig.call(|vm| vm.add_note_at_playhead(&accompaniment.id, "test note"));
    let notes = &rig
        .vm
        .practice()
        .unwrap()
        .accompaniment
        .as_ref()
        .unwrap()
        .notes;
    assert_eq!(notes.len(), 1);
    // ... and it landed in the store.
    let stored = repo.get_practice(&practice_id).unwrap();
    assert_eq!(stored.accompaniment.as_ref().unwrap().notes.len(), 1);

    rig.vm.dispose();
    rig.settle();
    assert!(!rig.vm.has_work_in_flight());
    let _ = std::fs::remove_dir_all(persistence.assets().root());
}
