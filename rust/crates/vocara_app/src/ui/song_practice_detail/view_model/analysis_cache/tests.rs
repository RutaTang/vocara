//! Port of `test/analysis_cache_test.dart` (the monitor interlock group
//! included), plus the analysis-cache assertions of
//! `playhead_repaint_test.dart` (a dropped half asked for again by the dock
//! and a folded-out lane; a clip drag's lane merge waiting for the move to
//! land) and the view-model half of `spectrogram_wanted_test.dart` (the job
//! carries its `wanted` predicate to the worker, never the cache).
//!
//! Dart's tests awaited the cache's futures over a stub service that
//! answered at once, parked a path's compute on a completer (`hold`), and
//! held the analysis gate's slots to keep a lane merge queued. Here a
//! [`Driver`] plays the screen: it runs each job the cache hands out — a
//! compute at once, as the stub's call snapshotted its answer at call time —
//! and delivers the results until nothing is left (Dart's `await`), holding
//! the results for held paths or kinds, and the merges while they are held.
//! The gate itself is exercised on threads with a private gate.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use vocara_model::models::audio_track::{AudioClip, AudioTrack, AudioTrackKind};
use vocara_model::models::spectrogram::Spectrogram;
use vocara_model::models::voice_metrics::{
    analyze_voice_profile, compute_ltas, Ltas, VoiceMetrics, VoiceProfile, VoiceProfileOptions,
};
use vocara_model::util::analysis_gate::AnalysisGate;
use vocara_model::util::dart_date_time::DateTime;
use vocara_model::util::lru_bytes_cache::LruBytesBudget;
use vocara_services::audio_dsp::AnalysisParams;
use vocara_services::engine::{EngineBindings, EngineError};
use vocara_services::graph_audio_playback_service::GraphAudioPlaybackService;
use vocara_services::mic_permission;
use vocara_services::monitor_service::{LiveMonitor, MonitorService};
use vocara_services::spectrogram_service::{SpectrogramAnalyses, TakeAnalysisBundle};

use super::*;

// --- Fakes ----------------------------------------------------------------------

/// Serves canned per-path analysis, so the cache's compute paths run for real
/// (per-clip caches, aggregate merge) without files.
#[derive(Default)]
struct Stub {
    s: Mutex<StubState>,
}

#[derive(Default)]
struct StubState {
    metrics_by_path: HashMap<String, Arc<VoiceMetrics>>,
    specs_by_path: HashMap<String, Arc<Spectrogram>>,
    /// Derived views served with a path's pair, standing in for the ones the
    /// real service computes beside it; absent = none served.
    ltas_by_path: HashMap<String, Arc<Ltas>>,
    profile_by_path: HashMap<String, Arc<VoiceProfile>>,
    /// How many times each half was computed on its own (the EQ display
    /// spectrogram counts as a spectrogram, as in Dart).
    spec_calls: usize,
    metrics_calls: usize,
    /// How many times the pair compute ran.
    both_calls: usize,
}

impl Stub {
    fn new(paths: &[&str], frames: usize) -> Arc<Stub> {
        let stub = Stub::default();
        {
            let mut s = stub.s();
            for p in paths {
                s.metrics_by_path.insert(p.to_string(), metrics(frames));
                s.specs_by_path.insert(p.to_string(), spec(frames, 2));
            }
        }
        Arc::new(stub)
    }

    fn s(&self) -> MutexGuard<'_, StubState> {
        self.s.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn calls(&self) -> (usize, usize, usize) {
        let s = self.s();
        (s.both_calls, s.spec_calls, s.metrics_calls)
    }

    fn decodes(&self) -> usize {
        let (b, s, m) = self.calls();
        b + s + m
    }
}

impl SpectrogramAnalyses for Stub {
    fn compute_from_file(
        &self,
        path: &str,
        _params: AnalysisParams,
        _wanted: Option<&dyn Fn() -> bool>,
    ) -> io::Result<Option<Arc<Spectrogram>>> {
        let mut s = self.s();
        s.spec_calls += 1;
        Ok(s.specs_by_path.get(path).cloned())
    }

    fn compute_metrics_from_file(
        &self,
        path: &str,
        _params: AnalysisParams,
        _wanted: Option<&dyn Fn() -> bool>,
    ) -> io::Result<Option<Arc<VoiceMetrics>>> {
        let mut s = self.s();
        s.metrics_calls += 1;
        Ok(s.metrics_by_path.get(path).cloned())
    }

    fn compute_both_from_file(
        &self,
        path: &str,
        _params: AnalysisParams,
        _wanted: Option<&dyn Fn() -> bool>,
    ) -> io::Result<Option<TakeAnalysisBundle>> {
        let mut s = self.s();
        s.both_calls += 1;
        Ok(Some(TakeAnalysisBundle {
            spectrogram: s.specs_by_path.get(path).cloned(),
            metrics: s.metrics_by_path.get(path).cloned(),
            ltas: s.ltas_by_path.get(path).cloned(),
            profile: s.profile_by_path.get(path).cloned(),
        }))
    }
}

/// Counts the live monitor's starts and stops.
#[derive(Default)]
struct FakeMonitor {
    starts: AtomicUsize,
    stops: AtomicUsize,
    fed: Mutex<Vec<f32>>,
}

impl LiveMonitor for FakeMonitor {
    fn start(&self) {
        self.starts.fetch_add(1, Ordering::SeqCst);
    }

    fn feed_samples(&self, samples: &[f32]) {
        self.fed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend_from_slice(samples);
    }

    fn stop(&self) {
        self.stops.fetch_add(1, Ordering::SeqCst);
    }

    fn active(&self) -> bool {
        false
    }
}

/// Capture-side fake of the native engine: counts start/stop_capture so the
/// live-monitor interlock tests can assert the pump never armed, and serves
/// scripted input frames.
#[derive(Default)]
struct FakeCapture {
    starts: AtomicUsize,
    stops: AtomicUsize,
    input: Mutex<Vec<f32>>,
}

impl EngineBindings for FakeCapture {
    fn sample_rate(&self) -> f64 {
        48_000.0
    }

    fn destroy(&self) {}

    fn start_capture(&self) -> Result<(), EngineError> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn stop_capture(&self) -> Result<(), EngineError> {
        self.stops.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn input_frames(&self, out: &mut [f32]) -> usize {
        let mut input = self.input.lock().unwrap_or_else(|e| e.into_inner());
        let n = input.len().min(out.len());
        out[..n].copy_from_slice(&input[..n]);
        input.drain(..n);
        n / 2
    }
}

/// The host: the selection, the model's tracks and the transport, as the
/// view model's closures read them.
#[derive(Default)]
struct Host {
    selected: Option<Arc<AudioTrack>>,
    clip_id: Option<String>,
    tracks: HashMap<String, Arc<AudioTrack>>,
    moving: HashSet<String>,
    playing: bool,
    recording: AtomicBool,
    pauses: AtomicUsize,
    /// REC arms during the pause.
    record_on_pause: bool,
    position: Duration,
}

impl Host {
    fn selecting(track: &Arc<AudioTrack>) -> Host {
        let mut host = Host::default();
        host.select(track);
        host
    }

    fn select(&mut self, track: &Arc<AudioTrack>) {
        self.selected = Some(Arc::clone(track));
        self.tracks.insert(track.id.clone(), Arc::clone(track));
    }

    fn with_tracks(tracks: &[&Arc<AudioTrack>]) -> Host {
        let mut host = Host::default();
        for t in tracks {
            host.tracks.insert(t.id.clone(), Arc::clone(t));
        }
        host
    }
}

impl AnalysisHost for Host {
    fn selected_track(&self) -> Option<Arc<AudioTrack>> {
        self.selected.clone()
    }

    fn selected_clip_id(&self) -> Option<&str> {
        self.clip_id.as_deref()
    }

    fn track_by_id(&self, id: &str) -> Option<Arc<AudioTrack>> {
        self.tracks.get(id).cloned()
    }

    fn is_moving_live(&self, track_id: &str) -> bool {
        self.moving.contains(track_id)
    }

    fn is_playing(&self) -> bool {
        self.playing
    }

    fn is_recording(&self) -> bool {
        self.recording.load(Ordering::SeqCst)
    }

    fn pause_playback(&self) {
        self.pauses.fetch_add(1, Ordering::SeqCst);
        if self.record_on_pause {
            self.recording.store(true, Ordering::SeqCst);
        }
    }

    fn position(&self) -> Duration {
        self.position
    }
}

/// Plays the screen: runs the jobs, delivers their results (see the module
/// docs), and keeps the other effects.
#[derive(Default)]
struct Driver {
    ready: VecDeque<JobDone>,
    /// Paths whose results are held (Dart's `hold`), with the held results.
    hold_paths: HashSet<String>,
    held: Vec<(String, JobDone)>,
    /// Job kinds whose results are held (Dart's gated metrics stub).
    hold_kinds: HashSet<JobKind>,
    held_kinds: Vec<(JobKind, JobDone)>,
    /// Lane merges are parked, not run (the gate's slots are taken).
    hold_merges: bool,
    merges: Vec<AnalysisJob>,
    /// The `wanted` predicate of the last pair compute.
    last_wanted: Option<WantedProbe>,
    mic_requests: usize,
    toasts: Vec<String>,
    retries: Vec<Duration>,
}

impl Driver {
    fn push(&mut self, effects: Vec<AnalysisEffect>) {
        for effect in effects {
            match effect {
                AnalysisEffect::Run(job) => self.start(job),
                AnalysisEffect::RequestMicPermission => self.mic_requests += 1,
                AnalysisEffect::Toast(message) => self.toasts.push(message),
                AnalysisEffect::LaneHealRetry { after } => self.retries.push(after),
            }
        }
    }

    fn start(&mut self, job: AnalysisJob) {
        let kind = job.kind();
        if kind == JobKind::MergeLane && self.hold_merges {
            self.merges.push(job);
            return;
        }
        if kind == JobKind::Both {
            self.last_wanted = Some(job.wanted_probe());
        }
        let path = job.path().map(str::to_string);
        let done = job.run();
        match path {
            Some(path) if self.hold_paths.contains(&path) => self.held.push((path, done)),
            _ if self.hold_kinds.contains(&kind) => self.held_kinds.push((kind, done)),
            _ => self.ready.push_back(done),
        }
    }

    /// Delivers every ready result, and the results of what those start,
    /// until nothing is left (Dart's `await` of the call).
    fn settle(&mut self, cache: &mut AnalysisCache, host: &Host) {
        while let Some(done) = self.ready.pop_front() {
            let effects = cache.job_done(done, host);
            self.push(effects);
        }
    }

    fn go(&mut self, cache: &mut AnalysisCache, host: &Host, effects: Vec<AnalysisEffect>) {
        self.push(effects);
        self.settle(cache, host);
    }

    fn hold(&mut self, path: &str) {
        self.hold_paths.insert(path.to_string());
    }

    /// Completes a held path (and stops holding it).
    fn release(&mut self, path: &str) {
        self.hold_paths.remove(path);
        let (released, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.held)
            .into_iter()
            .partition(|(p, _)| p == path);
        self.held = kept;
        self.ready.extend(released.into_iter().map(|(_, d)| d));
    }

    fn release_kind(&mut self, kind: JobKind) {
        self.hold_kinds.remove(&kind);
        let (released, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.held_kinds)
            .into_iter()
            .partition(|(k, _)| *k == kind);
        self.held_kinds = kept;
        self.ready.extend(released.into_iter().map(|(_, d)| d));
    }

    /// Frees the gate: the parked merges run, and later ones run at once.
    fn release_merges(&mut self) {
        self.hold_merges = false;
        for job in std::mem::take(&mut self.merges) {
            self.ready.push_back(job.run());
        }
    }
}

// --- Builders ---------------------------------------------------------------------

// Test framing: hop=100 @ 1 kHz → one frame per 100 ms, so a 1000 ms clip is
// exactly 10 frames and offsets land on whole frames (matches the merge
// tests).
const HOP: i64 = 100;
const SR: i64 = 1000;

fn metrics(frames: usize) -> Arc<VoiceMetrics> {
    Arc::new(metrics_value(frames))
}

fn metrics_value(frames: usize) -> VoiceMetrics {
    let flat = |v: f32| vec![v; frames];
    VoiceMetrics {
        frames,
        hop: HOP,
        fft_size: 2048,
        sample_rate: SR,
        ring: flat(0.0),
        tilt: flat(0.0),
        cpp: flat(0.0),
        loudness: flat(0.0),
        // Voiced (finite F0) + active so the aggregate merge folds every
        // frame in.
        f0: flat(200.0),
        f1: flat(f32::NAN),
        f2: flat(f32::NAN),
        f3: flat(f32::NAN),
        h1h2: flat(f32::NAN),
        active: vec![1; frames],
        ring_avg: 0.0,
        tilt_avg: 0.0,
        cpp_avg: 0.0,
        peak_dbfs: f64::NAN,
        clipped_fraction: 0.0,
        snr_db: f64::NAN,
    }
}

fn spec(frames: usize, bins: usize) -> Arc<Spectrogram> {
    Arc::new(Spectrogram {
        frames,
        bins,
        max_hz: 5000.0,
        sample_rate: SR,
        fft_size: 2048,
        hop: HOP,
        db: vec![0.0; frames * bins],
        min_db: -60.0,
        max_db: 0.0,
    })
}

fn lane_with(clips: Vec<AudioClip>, id: &str) -> Arc<AudioTrack> {
    Arc::new(AudioTrack {
        clips,
        ..AudioTrack::new(
            id,
            AudioTrackKind::Vocal,
            "Lane",
            DateTime::local_ymd(2026, 1, 1),
        )
    })
}

fn lane(clips: Vec<AudioClip>) -> Arc<AudioTrack> {
    lane_with(clips, "lane")
}

fn clip(id: &str, start_ms: i64) -> AudioClip {
    AudioClip {
        start_ms,
        ..AudioClip::new(id, format!("{id}.wav"), 1000)
    }
}

/// A cache over `stub`, on a private budget by default so tests never evict
/// each other's entries through the app-wide one.
fn cache_with(stub: &Arc<Stub>, options: AnalysisCacheOptions) -> AnalysisCache {
    let options = AnalysisCacheOptions {
        cache_budget: Some(
            options
                .cache_budget
                .unwrap_or_else(|| LruBytesBudget::new(1 << 30)),
        ),
        ..options
    };
    AnalysisCache::with_options(
        Arc::clone(stub) as Arc<dyn SpectrogramAnalyses>,
        Arc::new(MonitorService::default()),
        None,
        options,
    )
}

fn cache(stub: &Arc<Stub>) -> AnalysisCache {
    cache_with(stub, AnalysisCacheOptions::default())
}

fn budget(bytes: usize) -> AnalysisCacheOptions {
    AnalysisCacheOptions {
        cache_budget: Some(LruBytesBudget::new(bytes)),
        ..AnalysisCacheOptions::default()
    }
}

/// A gate of its own, so a test holding its slots never stalls another
/// test's merges (Dart's tests ran one at a time on the app-wide gate).
fn private_gate() -> &'static AnalysisGate {
    Box::leak(Box::new(AnalysisGate::new(2)))
}

/// Waits (bounded) for `done` to hold.
fn wait_for(done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(1));
    }
}

/// Takes `n` of `gate`'s slots on threads until each sender is sent to.
fn hold_slots(
    gate: &'static AnalysisGate,
    n: usize,
) -> Vec<(mpsc::Sender<()>, thread::JoinHandle<()>)> {
    let held: Vec<_> = (0..n)
        .map(|_| {
            let (tx, rx) = mpsc::channel::<()>();
            let handle = thread::spawn(move || {
                gate.run(|| {
                    let _ = rx.recv();
                });
            });
            (tx, handle)
        })
        .collect();
    wait_for(|| gate.running() == n);
    held
}

// --- Tests --------------------------------------------------------------------------

#[test]
fn an_ltas_read_before_the_metrics_landed_is_rebuilt_once_they_do() {
    // The LTAS tab renders as soon as the spectrogram is in (the inline lane
    // may have computed it alone), and compute_ltas drops its voiced-frame
    // gate while metrics are missing. That ungated result was memoized and
    // never replaced, so silence frames stayed averaged in for the session.
    let mut m = metrics_value(20);
    m.active[10..20].fill(0); // 10 voiced
    let stub = Stub::new(&[], 0);
    stub.s()
        .metrics_by_path
        .insert("c1.wav".into(), Arc::new(m));
    stub.s().specs_by_path.insert("c1.wav".into(), spec(20, 2));
    let track = lane(vec![clip("c1", 0)]);
    let host = Host::selecting(&track);
    let mut cache = cache(&stub);
    let mut d = Driver::default();

    // The spectrogram held alone: the byte budget let the metrics half go.
    let e = cache.ensure_spectrogram_for(&track.id, &host);
    d.go(&mut cache, &host, e);
    cache.debug_drop_half("c1.wav", true);
    d.hold_kinds.insert(JobKind::Metrics); // metrics pending
    let e = cache.ensure_selected_spectrogram(&host);
    d.go(&mut cache, &host, e);
    assert_eq!(
        cache.selected_ltas(&host).map(|l| l.frame_count),
        Some(20),
        "ungated meanwhile"
    );

    d.release_kind(JobKind::Metrics);
    d.settle(&mut cache, &host);
    assert!(cache.selected_metrics(&host).is_some());
    assert_eq!(
        cache.selected_ltas(&host).map(|l| l.frame_count),
        Some(10),
        "voiced-gated now"
    );
}

// --- aggregate_key_for -------------------------------------------------------------

#[test]
fn aggregate_key_embeds_the_ordered_clip_ids_their_timeline_starts_and_files() {
    let key = AnalysisCache::aggregate_key_for(&lane(vec![clip("c1", 0), clip("c2", 5000)]));
    assert_eq!(key, "agg:lane#c1@0:c1.wav,c2@5000:c2.wav");
}

#[test]
fn a_clip_whose_bytes_arrive_after_its_lane_was_analyzed_is_analyzed_again() {
    // A synced clip's row lands before its bytes, so it hydrates with an
    // asset:// ref that analyzes to nothing. The next save or reload re-reads
    // it at its pool path, with the same id and start: a key that ignored the
    // path kept serving the lane without it for the rest of the session.
    let missing = AudioClip {
        start_ms: 0,
        ..AudioClip::new("c1", "asset://a1", 1000)
    };
    let c2 = clip("c2", 2000);
    let stub = Stub::new(&["c1.wav", "c2.wav"], 10);
    let mut host = Host::selecting(&lane(vec![missing.clone(), c2.clone()]));
    let mut cache = cache(&stub);
    let mut d = Driver::default();

    let e = cache.ensure_selected_spectrogram(&host);
    d.go(&mut cache, &host, e);
    // Only c2 analyzed: c1 is an honest gap while its bytes are missing.
    let merged = cache.selected_metrics(&host).expect("the lane analyzed");
    assert!(merged.active[..10].iter().all(|&a| a == 0));

    // The bytes landed and a re-read hydrated the clip at a real path.
    host.select(&lane(vec![
        AudioClip {
            audio_file_path: "c1.wav".into(),
            ..missing
        },
        c2,
    ]));
    let e = cache.ensure_selected_spectrogram(&host);
    d.go(&mut cache, &host, e);
    let merged = cache.selected_metrics(&host).expect("the lane analyzed");
    assert!(
        merged.active[..10].iter().any(|&a| a != 0),
        "the clip that arrived is part of the lane analysis now"
    );
}

#[test]
fn aggregate_key_self_invalidates_when_a_clip_moves() {
    let before = AnalysisCache::aggregate_key_for(&lane(vec![clip("c1", 0), clip("c2", 5000)]));
    let after = AnalysisCache::aggregate_key_for(&lane(vec![clip("c1", 0), clip("c2", 6000)]));
    assert_ne!(before, after);
}

#[test]
fn aggregate_key_self_invalidates_when_the_clip_set_or_order_changes() {
    let two = AnalysisCache::aggregate_key_for(&lane(vec![clip("c1", 0), clip("c2", 5000)]));
    let three = AnalysisCache::aggregate_key_for(&lane(vec![
        clip("c1", 0),
        clip("c2", 5000),
        clip("c3", 8000),
    ]));
    let swapped = AnalysisCache::aggregate_key_for(&lane(vec![clip("c2", 5000), clip("c1", 0)]));
    assert_ne!(two, three);
    assert_ne!(two, swapped);
}

#[test]
fn the_agg_prefix_keeps_aggregate_keys_disjoint_from_file_paths() {
    assert!(AnalysisCache::aggregate_key_for(&lane(vec![clip("c1", 0)])).starts_with("agg:"));
}

// --- analysis subject axis -----------------------------------------------------------

#[test]
fn single_clip_lane_the_axis_is_the_clip_span_and_the_lane_span() {
    let stub = Stub::new(&["c1.wav"], 10);
    let m1 = stub.s().metrics_by_path["c1.wav"].clone();
    let track = lane(vec![clip("c1", 2000)]);
    let host = Host::selecting(&track);
    let mut cache = cache(&stub);
    let mut d = Driver::default();

    assert_eq!(cache.analysis_start_ms(&host), 2000);
    assert_eq!(cache.analysis_duration_ms(&host), 1000);
    // For a single-clip lane the one clip IS the lane span, so the axis is
    // provably unchanged from the old lane-span mapping.
    assert_eq!(cache.analysis_start_ms(&host), track.start_ms());
    assert_eq!(cache.analysis_duration_ms(&host), track.duration_ms());

    let e = cache.ensure_selected_spectrogram(&host);
    d.go(&mut cache, &host, e);
    assert!(Arc::ptr_eq(&cache.selected_metrics(&host).unwrap(), &m1));
}

#[test]
fn multi_clip_lane_with_no_clip_selected_the_axis_is_the_lane_span() {
    let stub = Stub::new(&["c1.wav", "c2.wav"], 10);
    let (m1, m2) = {
        let s = stub.s();
        (
            s.metrics_by_path["c1.wav"].clone(),
            s.metrics_by_path["c2.wav"].clone(),
        )
    };
    let track = lane(vec![clip("c1", 1000), clip("c2", 5000)]);
    let host = Host::selecting(&track);
    let mut cache = cache(&stub);
    let mut d = Driver::default();

    assert_eq!(cache.analysis_start_ms(&host), track.start_ms()); // 1000
    assert_eq!(cache.analysis_duration_ms(&host), track.duration_ms()); // 5000

    let e = cache.ensure_selected_spectrogram(&host);
    d.go(&mut cache, &host, e);
    let merged = cache.selected_metrics(&host).expect("merged");
    assert!(!Arc::ptr_eq(&merged, &m1));
    assert!(!Arc::ptr_eq(&merged, &m2));
    // The aggregate spans the whole lane (5000 ms → 50 frames at 100 ms),
    // matching the lane-span axis the getters report.
    assert_eq!(merged.frames, 50);
}

#[test]
fn a_lane_aggregate_comes_with_the_ltas_and_profile_its_pair_gives() {
    // Merged (and derived) on a worker now; the dock's getters must still
    // answer exactly what computing from the merged pair gives.
    let stub = Stub::new(&["c1.wav", "c2.wav"], 10);
    let track = lane(vec![clip("c1", 1000), clip("c2", 5000)]);
    let host = Host::selecting(&track);
    let mut cache = cache(&stub);
    let mut d = Driver::default();
    let e = cache.ensure_selected_spectrogram(&host);
    d.go(&mut cache, &host, e);
    let spec = cache.selected_spectrogram(&host).unwrap();
    let metrics = cache.selected_metrics(&host).unwrap();
    let ltas = cache.selected_ltas(&host).unwrap();
    let fresh = compute_ltas(&spec, Some(&metrics));
    assert_eq!(ltas.db, fresh.db);
    assert_eq!(ltas.alpha_ratio.to_bits(), fresh.alpha_ratio.to_bits());
    assert_eq!(
        ltas.singers_formant_db.to_bits(),
        fresh.singers_formant_db.to_bits()
    );
    assert_eq!(ltas.frame_count, fresh.frame_count);
    let profile = cache.selected_profile(&host).unwrap();
    let fresh = analyze_voice_profile(&metrics, VoiceProfileOptions::default());
    assert_eq!(
        (
            profile.low_midi,
            profile.high_midi,
            profile.tess_lo_midi,
            profile.tess_hi_midi
        ),
        (
            fresh.low_midi,
            fresh.high_midi,
            fresh.tess_lo_midi,
            fresh.tess_hi_midi
        )
    );
}

#[test]
fn a_clip_evicted_while_a_later_clip_computes_is_not_baked_into_the_lane_as_silence() {
    // The per-clip caches are byte-budgeted, so analyzing clip N can evict
    // clip 1 before the merge reads it back — and the merge treats a missing
    // part as SILENCE, under a key only a clip-set change can invalidate. A
    // budget that fits ONE clip's pair (10 frames: 410 B of metrics + 80 B
    // of spectrogram) makes c2's analysis push c1 out at exactly that point.
    let stub = Stub::new(&["c1.wav", "c2.wav"], 10);
    let track = lane(vec![clip("c1", 1000), clip("c2", 5000)]);
    let host = Host::selecting(&track);
    let mut cache = cache_with(&stub, budget(490));
    let mut d = Driver::default();

    let e = cache.ensure_selected_spectrogram(&host);
    d.go(&mut cache, &host, e);

    // c1's frames survived the eviction because the loop held them, so the
    // lane still spans both clips (5000 ms → 50 frames at 100 ms/frame).
    let merged = cache.selected_metrics(&host).expect("merged");
    assert_eq!(merged.frames, 50);
    // And c1's own frames carry real analysis rather than the merge's
    // silence fill.
    assert!(merged.active[..10].iter().any(|&a| a != 0));
}

#[test]
fn a_lane_whose_clip_was_already_computing_still_settles() {
    // Select a clip (its compute starts), then click the lane header before
    // it lands. The aggregate loop reached that clip mid-compute; skipping
    // ahead of it produced a lane the merge refused to cache — correctly, it
    // was incomplete — with nothing scheduled to ever try again, so the dock
    // sat on "analysis is available for recorded takes" forever.
    let stub = Stub::new(&["c1.wav", "c2.wav"], 10);
    let track = lane(vec![clip("c1", 1000), clip("c2", 5000)]);
    let mut host = Host::selecting(&track);
    host.clip_id = Some("c1".into());
    let mut cache = cache(&stub);
    let mut d = Driver::default();
    // c1 is the SLOW one: it is still in flight when the aggregate loop asks
    // for it, and only lands after c2 has finished.
    d.hold("c1.wav");

    let e = cache.ensure_selected_spectrogram(&host);
    d.go(&mut cache, &host, e);
    // Lane header clicked: selection widens to the whole lane.
    host.clip_id = None;
    let e = cache.ensure_selected_spectrogram(&host);
    d.go(&mut cache, &host, e);
    assert!(
        cache.selected_metrics(&host).is_none(),
        "c1 is still computing"
    );

    d.release("c1.wav");
    d.settle(&mut cache, &host);

    let merged = cache
        .selected_metrics(&host)
        .expect("the lane must not settle blank");
    assert_eq!(merged.frames, 50);
    // The joiner reused c1's work rather than launching a second decode.
    assert_eq!(stub.s().both_calls, 2);
}

#[test]
fn a_clip_evicted_mid_compute_is_reanalyzed_not_answered_with_the_old_result() {
    // Re-analyze exists for a file that changed underneath, and it evicts
    // each clip while the dock may still be computing it. That compute read
    // the OLD bytes: writing its result back leaves the take showing the
    // superseded analysis under a key nothing invalidates again.
    let stub = Stub::new(&["c1.wav"], 10);
    let track = lane(vec![clip("c1", 0)]);
    let host = Host::selecting(&track);
    let mut cache = cache(&stub);
    let mut d = Driver::default();
    d.hold("c1.wav");

    let e = cache.ensure_selected_spectrogram(&host);
    d.go(&mut cache, &host, e);

    // The file is replaced on disk and the take re-analyzed.
    cache.evict_analysis("c1.wav", true);
    stub.s()
        .metrics_by_path
        .insert("c1.wav".into(), metrics(20));
    stub.s().specs_by_path.insert("c1.wav".into(), spec(20, 2));
    d.release("c1.wav");
    d.settle(&mut cache, &host);

    assert_eq!(
        cache.selected_metrics(&host).map(|m| m.frames),
        Some(20),
        "the evicted analysis must not be served as the new one"
    );
    assert_eq!(stub.s().both_calls, 2, "the discarded work must be redone");
}

#[test]
fn an_eviction_mid_compute_clears_a_stale_unavailable_latch_instead_of_relatching_it() {
    // Same window, failure side: the pre-eviction read found nothing (the
    // file was mid-write), and re-latching after the evict made the take
    // permanently "no analysis" even though the replacement is fine.
    let stub = Stub::new(&[], 0);
    let track = lane(vec![clip("c1", 0)]);
    let host = Host::selecting(&track);
    let mut cache = cache(&stub);
    let mut d = Driver::default();
    d.hold("c1.wav");

    let e = cache.ensure_selected_spectrogram(&host);
    d.go(&mut cache, &host, e);

    cache.evict_analysis("c1.wav", true);
    stub.s()
        .metrics_by_path
        .insert("c1.wav".into(), metrics(10));
    stub.s().specs_by_path.insert("c1.wav".into(), spec(10, 2));
    d.release("c1.wav");
    d.settle(&mut cache, &host);

    assert!(!cache.selected_metrics_unavailable(&host));
    assert_eq!(cache.selected_metrics(&host).map(|m| m.frames), Some(10));
}

// Queued behind the gate, a pair whose screen closed or whose file changed
// still ran its full-take compute for no one.
#[test]
fn a_queued_pair_compute_stops_being_wanted_once_the_cache_closes_or_the_file_changes() {
    let stub = Stub::new(&["c1.wav"], 10);
    let track = lane(vec![clip("c1", 0)]);
    let host = Host::selecting(&track);
    let mut cache = cache(&stub);
    let mut d = Driver::default();
    d.hold("c1.wav");

    let e = cache.ensure_selected_spectrogram(&host);
    d.go(&mut cache, &host, e);
    let wanted = d.last_wanted.clone().unwrap();
    assert!(wanted.is_wanted());
    cache.evict_analysis("c1.wav", false);
    assert!(!wanted.is_wanted(), "its file changed");
    d.release("c1.wav");
    d.settle(&mut cache, &host);

    d.hold("c1.wav");
    let e = cache.ensure_selected_spectrogram(&host);
    d.go(&mut cache, &host, e);
    let wanted = d.last_wanted.clone().unwrap();
    assert!(wanted.is_wanted());
    cache.dispose();
    assert!(!wanted.is_wanted(), "its screen closed");
    d.release("c1.wav");
    d.settle(&mut cache, &host);
}

#[test]
fn a_clip_evicted_mid_compute_because_it_left_the_model_is_not_analyzed_again() {
    // Redoing the discarded work is for re-analyze, where the same path now
    // holds new bytes. A removed file's redo just re-decoded it into one of
    // the app-wide analysis slots, for a cache entry nothing reads.
    let stub = Stub::new(&["c1.wav"], 10);
    let track = lane(vec![clip("c1", 0)]);
    let host = Host::selecting(&track);
    let mut cache = cache(&stub);
    let mut d = Driver::default();
    d.hold("c1.wav");

    let e = cache.ensure_selected_spectrogram(&host);
    d.go(&mut cache, &host, e);
    cache.evict_analysis("c1.wav", false);
    d.release("c1.wav");
    d.settle(&mut cache, &host);

    assert_eq!(stub.s().both_calls, 1);
}

#[test]
fn a_specific_clip_of_a_multi_clip_lane_selected_the_axis_is_that_clips_span() {
    let stub = Stub::new(&["c1.wav", "c2.wav"], 10);
    let m2 = stub.s().metrics_by_path["c2.wav"].clone();
    let track = lane(vec![clip("c1", 1000), clip("c2", 5000)]);
    let mut host = Host::selecting(&track);
    host.clip_id = Some("c2".into());
    let mut cache = cache(&stub);
    let mut d = Driver::default();

    // Clip-local subject: the axis follows the clip, not the lane.
    assert_eq!(cache.analysis_start_ms(&host), 5000);
    assert_eq!(cache.analysis_duration_ms(&host), 1000);
    let e = cache.ensure_selected_spectrogram(&host);
    d.go(&mut cache, &host, e);
    assert!(Arc::ptr_eq(&cache.selected_metrics(&host).unwrap(), &m2));

    // Deselecting the clip flips both the served object and the axis back to
    // the lane-aggregate subject together.
    host.clip_id = None;
    assert_eq!(cache.analysis_start_ms(&host), 1000);
    assert_eq!(cache.analysis_duration_ms(&host), 5000);
    assert!(cache
        .selected_metrics(&host)
        .is_none_or(|m| !Arc::ptr_eq(&m, &m2)));
}

#[test]
fn a_stale_clip_id_falls_back_to_the_aggregate_subject_axis_included() {
    let stub = Stub::new(&[], 0);
    let track = lane(vec![clip("c1", 1000), clip("c2", 5000)]);
    let mut host = Host::selecting(&track);
    host.clip_id = Some("gone".into());
    let cache = cache(&stub);
    assert_eq!(cache.analysis_start_ms(&host), track.start_ms());
    assert_eq!(cache.analysis_duration_ms(&host), track.duration_ms());
}

#[test]
fn no_subject_reads_as_a_zero_axis() {
    let cache = cache(&Stub::new(&[], 0));
    let host = Host::default();
    assert_eq!(cache.analysis_start_ms(&host), 0);
    assert_eq!(cache.analysis_duration_ms(&host), 0);
}

// --- bottom dock chrome -----------------------------------------------------------

#[test]
fn the_dock_starts_closed_on_the_summary_tab_and_toggles_open() {
    let mut cache = cache(&Stub::new(&[], 0));
    let host = Host::default();
    assert!(!cache.dock_open());
    assert_eq!(cache.dock_tab(), DockTab::AnalysisSummary);
    let _ = cache.toggle_dock(&host);
    assert!(cache.dock_open());
    let _ = cache.toggle_dock(&host);
    assert!(!cache.dock_open());
}

#[test]
fn show_dock_tab_opens_the_dock_on_that_tab() {
    let mut cache = cache(&Stub::new(&[], 0));
    let _ = cache.show_dock_tab(DockTab::Pitch, &Host::default());
    assert!(cache.dock_open());
    assert_eq!(cache.dock_tab(), DockTab::Pitch);
}

#[test]
fn every_tab_but_the_mixer_analyzes_the_selected_take() {
    let mut cache = cache(&Stub::new(&[], 0));
    let host = Host::default();
    let _ = cache.show_dock_tab(DockTab::Mixer, &host);
    assert!(!cache.dock_shows_take());
    let _ = cache.show_dock_tab(DockTab::Spectrogram, &host);
    assert!(cache.dock_shows_take());
}

#[test]
fn only_the_spectral_pitch_timbre_family_and_the_vocal_tract_support_the_live_mic() {
    let mut cache = cache(&Stub::new(&[], 0));
    let host = Host::default();
    let live = [
        DockTab::Spectrogram,
        DockTab::Power,
        DockTab::Ltas,
        DockTab::Vowel,
        DockTab::Pitch,
        DockTab::Vibrato,
        DockTab::Timbre,
        DockTab::Registers,
        DockTab::VocalTract,
    ];
    for tab in DockTab::ALL {
        let _ = cache.show_dock_tab(tab, &host);
        assert_eq!(
            cache.active_tab_supports_live(),
            live.contains(&tab),
            "tab {tab:?}"
        );
    }
}

#[test]
fn set_dock_height_clamps_to_the_sane_resize_range() {
    let mut cache = cache(&Stub::new(&[], 0));
    cache.set_dock_height(40.0);
    assert_eq!(cache.dock_height(), 160.0);
    cache.set_dock_height(5000.0);
    assert_eq!(cache.dock_height(), 760.0);
    cache.set_dock_height(300.0);
    assert_eq!(cache.dock_height(), 300.0);
}

#[test]
fn dock_tab_names_are_darts() {
    // The DOCK_TAB demo hook names a tab as Dart's enum did.
    assert_eq!(DockTab::AnalysisSummary.name(), "analysisSummary");
    assert_eq!(DockTab::Registers.name(), "registers");
    assert_eq!(DockTab::Mixer.name(), "mixer");
}

// --- live monitor vs recording interlock ---------------------------------------

struct Live {
    cache: AnalysisCache,
    capture: Arc<FakeCapture>,
    monitor: Arc<FakeMonitor>,
    driver: Driver,
}

fn live_cache() -> Live {
    let capture = Arc::new(FakeCapture::default());
    let monitor = Arc::new(FakeMonitor::default());
    let engine = Arc::new(GraphAudioPlaybackService::with_bindings(
        Arc::clone(&capture) as Arc<dyn EngineBindings>,
        None,
    ));
    let cache = AnalysisCache::with_options(
        Stub::new(&[], 0),
        Arc::new(MonitorService::default()),
        Some(engine),
        AnalysisCacheOptions {
            live_monitor: Some(Arc::clone(&monitor) as Arc<dyn LiveMonitor>),
            cache_budget: Some(LruBytesBudget::new(1 << 30)),
            ..AnalysisCacheOptions::default()
        },
    );
    Live {
        cache,
        capture,
        monitor,
        driver: Driver::default(),
    }
}

impl Live {
    fn captures(&self) -> (usize, usize) {
        (
            self.capture.starts.load(Ordering::SeqCst),
            self.capture.stops.load(Ordering::SeqCst),
        )
    }

    fn monitor_starts(&self) -> usize {
        self.monitor.starts.load(Ordering::SeqCst)
    }

    fn set(&mut self, on: bool, host: &Host) {
        let e = self.cache.set_live_monitor(on, host);
        self.driver.push(e);
    }

    fn answer(&mut self, granted: bool, host: &Host) {
        assert!(self.driver.mic_requests > 0, "the microphone was asked for");
        self.driver.mic_requests -= 1;
        let e = self.cache.mic_permission_answered(granted, host);
        self.driver.push(e);
    }
}

#[test]
fn a_normal_live_start_arms_the_capture_once() {
    let mut live = live_cache();
    let host = Host::default();

    live.set(true, &host);
    live.answer(true, &host);
    assert!(live.cache.live_monitor());
    assert!(live.cache.capture_pump_active());
    assert_eq!(live.captures().0, 1);
    assert_eq!(live.monitor_starts(), 1);

    live.set(false, &host);
    assert!(!live.cache.live_monitor());
    assert!(!live.cache.capture_pump_active());
    assert_eq!(live.captures().1, 1);
}

#[test]
fn the_capture_pump_feeds_the_monitor_the_downmixed_input() {
    let mut live = live_cache();
    let host = Host::default();
    live.set(true, &host);
    live.answer(true, &host);
    *live.capture.input.lock().unwrap() = vec![0.5, 0.1, -0.2, 0.4];
    live.cache.pump_capture();
    let fed = live.monitor.fed.lock().unwrap().clone();
    assert_eq!(fed, vec![0.3, 0.1]);
    // An empty pull feeds nothing.
    live.cache.pump_capture();
    assert_eq!(live.monitor.fed.lock().unwrap().len(), 2);
}

#[test]
fn a_live_start_aborts_when_a_recording_started_during_the_pause() {
    // The take owns the capture. (Dart's pause was an await the start parked
    // on; the engine's pause returns at once here, so REC arms inside it.)
    let mut live = live_cache();
    let host = Host {
        playing: true,
        record_on_pause: true,
        ..Host::default()
    };

    live.set(true, &host);
    live.answer(true, &host);

    assert!(!live.cache.live_monitor());
    assert_eq!(live.captures().0, 0, "no pump under the take");
    assert_eq!(live.monitor_starts(), 0);
}

#[test]
fn set_live_monitor_false_cancels_a_live_start_parked_on_its_prompt() {
    // startRecording's interlock call: live_monitor is still false here, so
    // pre-fix this returned early and the parked start armed DURING the take.
    // (The window Dart's test parked in was the pause await; here the start
    // parks on its permission prompt, the pause returning at once.)
    let mut live = live_cache();
    let host = Host {
        playing: true,
        ..Host::default()
    };

    live.set(true, &host);
    live.set(false, &host);
    live.answer(true, &host);

    assert!(!live.cache.live_monitor());
    assert_eq!(live.captures().0, 0);
    assert_eq!(live.monitor_starts(), 0);
    assert_eq!(host.pauses.load(Ordering::SeqCst), 0);
}

#[test]
fn a_refused_microphone_leaves_the_capture_off_and_the_transport_playing_and_says_why() {
    let mut live = live_cache();
    let host = Host {
        playing: true,
        ..Host::default()
    };

    live.set(true, &host);
    live.answer(false, &host);

    assert!(!live.cache.live_monitor());
    assert_eq!(live.captures().0, 0);
    assert_eq!(live.monitor_starts(), 0);
    assert_eq!(host.pauses.load(Ordering::SeqCst), 0);
    assert_eq!(live.driver.toasts, vec![mic_permission::DENIED_MESSAGE]);
}

#[test]
fn a_live_start_cancelled_while_the_permission_prompt_is_up_stays_off() {
    let mut live = live_cache();
    let host = Host::default();

    live.set(true, &host);
    live.set(false, &host);
    live.answer(true, &host);

    assert!(!live.cache.live_monitor());
    assert_eq!(live.captures().0, 0);
    assert!(live.driver.toasts.is_empty());
}

#[test]
fn a_live_start_cannot_begin_while_recording() {
    let mut live = live_cache();
    let host = Host::default();
    host.recording.store(true, Ordering::SeqCst);
    live.set(true, &host);
    assert_eq!(live.driver.mic_requests, 0);
    assert!(!live.cache.live_monitor());
    assert_eq!(live.captures().0, 0);
}

#[test]
fn closing_the_dock_or_leaving_for_a_non_live_tab_stops_the_live_view() {
    let mut live = live_cache();
    let host = Host::default();
    let e = live.cache.show_dock_tab(DockTab::Spectrogram, &host);
    live.driver.push(e);
    live.set(true, &host);
    live.answer(true, &host);
    assert!(live.cache.live_monitor());
    let e = live.cache.show_dock_tab(DockTab::Range, &host);
    live.driver.push(e);
    assert!(!live.cache.live_monitor());
    assert_eq!(live.captures().1, 1);

    let e = live.cache.show_dock_tab(DockTab::Pitch, &host);
    live.driver.push(e);
    live.set(true, &host);
    live.answer(true, &host);
    let e = live.cache.set_dock_open(false, &host);
    live.driver.push(e);
    assert!(!live.cache.live_monitor());
    assert_eq!(live.captures().1, 2);
}

#[test]
fn dispose_stops_a_running_capture_and_the_pump() {
    let mut live = live_cache();
    let host = Host::default();
    live.set(true, &host);
    live.answer(true, &host);
    live.cache.dispose();
    assert!(!live.cache.capture_pump_active());
    assert_eq!(live.captures().1, 1);
}

// --- a folded-out lane on screen ------------------------------------------------
//
// Lanes compute when folded open or toggled. Data the shared byte budget let
// go (the dock on other long takes) or a clip move re-keyed left a lane
// empty, saying the take could not be analyzed, until toggled again.

struct Lanes {
    single: Arc<AudioTrack>,
    multi: Arc<AudioTrack>,
    odd: Arc<AudioTrack>,
}

fn lanes() -> (Lanes, Arc<Stub>, Host) {
    let l = Lanes {
        single: lane_with(vec![clip("s1", 0)], "S"),
        multi: lane_with(vec![clip("m1", 0), clip("m2", 2000)], "M"),
        odd: lane_with(vec![clip("x1", 0)], "X"), // nothing to analyze
    };
    let stub = Stub::new(&["s1.wav", "m1.wav", "m2.wav"], 10);
    let host = Host::with_tracks(&[&l.single, &l.multi, &l.odd]);
    (l, stub, host)
}

#[test]
fn a_folded_out_lane_asks_again_for_a_takes_analysis_the_budget_let_go() {
    let (_, stub, host) = lanes();
    let mut cache = cache(&stub);
    let mut d = Driver::default();
    let e = cache.toggle_track_expanded("S", &host);
    d.go(&mut cache, &host, e);
    assert!(cache.metrics_for("S", &host).is_some());
    assert!(!cache.lane_lacks_data("S", &host));

    cache.debug_drop_half("s1.wav", true);
    assert!(cache.lane_lacks_data("S", &host));
    let e = cache.heal_lane("S", &host);
    d.go(&mut cache, &host, e);
    assert!(cache.metrics_for("S", &host).is_some());
    assert!(!cache.lane_lacks_data("S", &host));
}

#[test]
fn a_folded_out_lane_asks_again_for_a_lane_merge_let_go() {
    let (_, stub, host) = lanes();
    let mut cache = cache(&stub);
    let mut d = Driver::default();
    let e = cache.toggle_track_expanded("M", &host);
    d.go(&mut cache, &host, e);
    assert!(cache.metrics_for("M", &host).is_some());

    cache.evict_analysis("m1.wav", false);
    assert!(cache.lane_lacks_data("M", &host));
    let e = cache.heal_lane("M", &host);
    d.go(&mut cache, &host, e);
    assert!(cache.metrics_for("M", &host).is_some());
}

#[test]
fn a_folded_out_lane_asks_for_the_same_take_at_most_once_in_a_while() {
    // More analysis on screen than the budget holds: lanes and the dock would
    // evict each other's in turn, one decode after another.
    let (_, stub, host) = lanes();
    let mut cache = cache(&stub);
    let mut d = Driver::default();
    let e = cache.toggle_track_expanded("S", &host);
    d.go(&mut cache, &host, e);
    cache.debug_drop_half("s1.wav", true);
    let e = cache.heal_lane("S", &host);
    d.go(&mut cache, &host, e);
    let computes = {
        let (b, _, m) = stub.calls();
        b + m
    };

    cache.debug_drop_half("s1.wav", true);
    let e = cache.heal_lane("S", &host);
    d.go(&mut cache, &host, e);
    let (b, _, m) = stub.calls();
    assert_eq!(b + m, computes);
    assert!(cache.metrics_for("S", &host).is_none());
}

/// A clock a test moves by hand.
fn fake_clock() -> (Arc<Mutex<Instant>>, AnalysisCacheOptions) {
    let clock = Arc::new(Mutex::new(Instant::now()));
    let read = Arc::clone(&clock);
    let options = AnalysisCacheOptions {
        now: Some(Box::new(move || *read.lock().unwrap())),
        ..AnalysisCacheOptions::default()
    };
    (clock, options)
}

#[test]
fn a_heal_the_interval_refused_is_asked_again_once_it_is_up() {
    // Nothing else may rebuild the lane: on an idle screen it sat empty.
    let (_, stub, host) = lanes();
    let (_clock, options) = fake_clock();
    let mut cache = cache_with(&stub, options);
    let mut d = Driver::default();
    let e = cache.toggle_track_expanded("S", &host);
    d.go(&mut cache, &host, e);
    cache.debug_drop_half("s1.wav", true);
    let e = cache.heal_lane("S", &host);
    d.go(&mut cache, &host, e);
    cache.debug_drop_half("s1.wav", true);
    let e = cache.heal_lane("S", &host); // refused: asked too recently
    d.go(&mut cache, &host, e);
    let notified = cache.revision();

    // The timer is the screen's: due after the rest of the interval (the
    // clock did not move, so all of it).
    assert_eq!(d.retries, vec![Duration::from_secs(30)]);
    assert_eq!(cache.revision(), notified);
    cache.lane_heal_retry_due();
    assert_eq!(
        cache.revision(),
        notified + 1,
        "the lanes rebuild and ask again"
    );
}

#[test]
fn heals_far_apart_each_still_earn_their_retry() {
    // Counted for the life of the screen, three heals an hour apart used up
    // the retries: the next lane let go just after a heal sat empty on an
    // idle screen for good.
    let (_, stub, host) = lanes();
    let (clock, options) = fake_clock();
    let mut cache = cache_with(&stub, options);
    let mut d = Driver::default();
    let e = cache.toggle_track_expanded("S", &host);
    d.go(&mut cache, &host, e);
    for _ in 0..3 {
        *clock.lock().unwrap() += Duration::from_secs(20 * 60);
        cache.debug_drop_half("s1.wav", true);
        let e = cache.heal_lane("S", &host);
        d.go(&mut cache, &host, e);
        assert!(cache.metrics_for("S", &host).is_some());
    }
    *clock.lock().unwrap() += Duration::from_secs(5);
    cache.debug_drop_half("s1.wav", true);
    let e = cache.heal_lane("S", &host); // refused: asked too recently
    d.go(&mut cache, &host, e);
    let notified = cache.revision();

    assert_eq!(d.retries, vec![Duration::from_secs(25)]);
    cache.lane_heal_retry_due();
    assert_eq!(
        cache.revision(),
        notified + 1,
        "the lanes rebuild and ask again"
    );
}

#[test]
fn a_subject_healed_three_times_in_a_row_earns_no_more_retries() {
    let (_, stub, host) = lanes();
    let (clock, options) = fake_clock();
    let mut cache = cache_with(&stub, options);
    let mut d = Driver::default();
    let e = cache.toggle_track_expanded("S", &host);
    d.go(&mut cache, &host, e);
    for _ in 0..3 {
        *clock.lock().unwrap() += Duration::from_secs(31);
        cache.debug_drop_half("s1.wav", true);
        let e = cache.heal_lane("S", &host);
        d.go(&mut cache, &host, e);
    }
    *clock.lock().unwrap() += Duration::from_secs(5);
    cache.debug_drop_half("s1.wav", true);
    let e = cache.heal_lane("S", &host);
    d.go(&mut cache, &host, e);
    assert!(d.retries.is_empty());
}

#[test]
fn a_folded_out_lane_does_not_ask_for_a_take_that_cannot_be_analyzed_or_one_folded_shut() {
    let (_, stub, host) = lanes();
    let mut cache = cache(&stub);
    let mut d = Driver::default();
    let e = cache.toggle_track_expanded("X", &host);
    d.go(&mut cache, &host, e);
    assert!(cache.metrics_for("X", &host).is_none());
    assert!(!cache.lane_lacks_data("X", &host), "settled: none");
    assert!(!cache.lane_lacks_data("S", &host), "never folded open");
}

// --- aggregate eviction -----------------------------------------------------------

fn two_lanes() -> (Arc<Stub>, Host) {
    let lane_a = lane_with(vec![clip("a1", 0), clip("a2", 2000)], "A");
    let lane_c = lane_with(vec![clip("c1", 0), clip("c2", 2000)], "C");
    let stub = Stub::new(&["a1.wav", "a2.wav", "c1.wav", "c2.wav"], 10);
    (stub, Host::with_tracks(&[&lane_a, &lane_c]))
}

#[test]
fn removing_one_lanes_file_keeps_the_analysis_of_a_lane_that_never_played_it() {
    // Deleting a clip or a take, a split, a merge or a re-analyze evicts the
    // files they touched — and with them EVERY lane aggregate, so another
    // take's folded-out pitch lane went blank (no data, no spinner) and was
    // never computed again until the lane was toggled.
    let (stub, host) = two_lanes();
    let mut cache = cache(&stub);
    let mut d = Driver::default();
    let e = cache.ensure_metrics_for("A", &host);
    d.go(&mut cache, &host, e);
    let e = cache.ensure_metrics_for("C", &host);
    d.go(&mut cache, &host, e);

    cache.evict_analysis("a1.wav", false);

    assert!(
        cache.metrics_for("A", &host).is_none(),
        "A folded the file in"
    );
    assert!(cache.metrics_for("C", &host).is_some(), "C never played it");
}

#[test]
fn a_lane_merge_in_flight_survives_an_eviction_of_a_file_it_does_not_fold() {
    let (stub, host) = two_lanes();
    let mut cache = cache(&stub);
    let mut d = Driver::default();
    d.hold("c2.wav");
    let e = cache.ensure_metrics_for("A", &host);
    d.go(&mut cache, &host, e);
    let e = cache.ensure_metrics_for("C", &host);
    d.go(&mut cache, &host, e);

    cache.evict_analysis("a1.wav", false);
    d.release("c2.wav");
    d.settle(&mut cache, &host);

    assert!(cache.metrics_for("C", &host).is_some());
}

#[test]
fn a_shown_lane_whose_merge_was_disowned_mid_flight_is_merged_again() {
    // Re-analyze evicts each clip while a folded-out lane may still be
    // merging it: the merge is rightly discarded, but nothing asked for the
    // lane again, so it sat empty until it was toggled.
    let (stub, host) = two_lanes();
    let mut cache = cache(&stub);
    let mut d = Driver::default();
    d.hold("c2.wav");
    let e = cache.toggle_track_expanded("C", &host); // the pitch lane computes the aggregate
    d.go(&mut cache, &host, e);

    cache.evict_analysis("c2.wav", true);
    stub.s()
        .metrics_by_path
        .insert("c2.wav".into(), metrics(20));
    d.release("c2.wav");
    d.settle(&mut cache, &host);

    assert!(!cache.metrics_loading_for("C", &host));
    let merged = cache
        .metrics_for("C", &host)
        .expect("the lane is analyzed again");
    // The re-analyzed c2 (20 frames from 2000 ms) now ends the lane span.
    assert_eq!(merged.frames, 40);
}

// --- lane merge admission -------------------------------------------------------

#[test]
fn dragging_a_clip_of_a_lane_selected_by_its_header_merges_the_lane_once_more_where_it_lands() {
    // The key embeds the clip starts, so every drag tick is a new key, and
    // the inspector asks for each one: every frame started its own merge,
    // copying all the lane's clip analyses into a fresh worker.
    let stub = Stub::new(&["c1.wav", "c2.wav"], 10);
    let at = |ms: i64| lane(vec![clip("c1", 0), clip("c2", ms)]);
    let mut host = Host::selecting(&at(2000));
    let mut cache = cache(&stub);
    // A real lane merge outlasts many frames; holding the analysis slots for
    // the drag stands in for that, deterministically.
    let mut d = Driver {
        hold_merges: true,
        ..Driver::default()
    };
    let ticks: Vec<_> = (0..20).map(|i| at(2000 + 100 * i)).collect();
    for t in &ticks {
        host.select(t);
        let e = cache.ensure_selected_spectrogram(&host);
        d.go(&mut cache, &host, e);
    }
    assert_eq!(d.merges.len(), 1, "one merge runs per lane");
    d.release_merges();
    d.settle(&mut cache, &host);

    let mut merged = 0;
    for t in &ticks {
        host.select(t);
        if cache.selected_metrics(&host).is_some() {
            merged += 1;
        }
    }
    assert!(cache.selected_metrics(&host).is_some(), "where it landed");
    assert!(merged <= 2, "the first + one rerun");
}

#[test]
fn a_lane_merge_waits_for_an_analysis_slot_like_any_heavy_compute() {
    // Its input is every clip's analysis, copied to a worker: outside the
    // app-wide gate any number of them ran at once.
    let gate = private_gate();
    let stub = Stub::new(&["c1.wav", "c2.wav"], 10);
    let track = lane(vec![clip("c1", 0), clip("c2", 2000)]);
    let host = Host::selecting(&track);
    let mut cache = cache_with(
        &stub,
        AnalysisCacheOptions {
            gate: Some(gate),
            ..AnalysisCacheOptions::default()
        },
    );
    let mut d = Driver {
        hold_merges: true,
        ..Driver::default()
    };
    let slots = hold_slots(gate, 2);

    let e = cache.ensure_selected_spectrogram(&host);
    d.go(&mut cache, &host, e);
    let job = d.merges.pop().expect("the merge was asked for");
    let (tx, rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        let _ = tx.send(job.run());
    });
    wait_for(|| gate.queued() == 1);
    assert_eq!(gate.queued(), 1, "the merge waits for a slot");
    assert!(rx.try_recv().is_err());
    assert!(cache.selected_metrics(&host).is_none(), "no slot was free");
    assert!(cache.spectrogram_loading(&host));

    for (tx, handle) in slots {
        let _ = tx.send(());
        handle.join().unwrap();
    }
    worker.join().unwrap();
    let e = cache.job_done(rx.recv().unwrap(), &host);
    d.go(&mut cache, &host, e);
    assert!(cache.selected_metrics(&host).is_some());
}

#[test]
fn a_lane_merge_still_queued_when_the_screen_goes_away_gives_its_slot_to_the_next_analysis() {
    // Admitted after dispose, it copied every clip's analysis to a worker
    // for a cache with no reader, ahead of the next screen's work.
    let gate = private_gate();
    let stub = Stub::new(&["c1.wav", "c2.wav"], 10);
    let track = lane(vec![clip("c1", 0), clip("c2", 2000)]);
    let host = Host::selecting(&track);
    let mut cache = cache_with(
        &stub,
        AnalysisCacheOptions {
            gate: Some(gate),
            ..AnalysisCacheOptions::default()
        },
    );
    let mut d = Driver {
        hold_merges: true,
        ..Driver::default()
    };
    let mut slots = hold_slots(gate, 2);
    let e = cache.ensure_selected_spectrogram(&host);
    d.go(&mut cache, &host, e);
    let job = d.merges.pop().expect("the merge was asked for");
    let merging = thread::spawn(move || job.run());
    wait_for(|| gate.queued() == 1);
    assert_eq!(gate.queued(), 1, "the merge waits for a slot");
    cache.dispose();

    let next_ran = Arc::new(AtomicBool::new(false));
    let next = {
        let next_ran = Arc::clone(&next_ran);
        thread::spawn(move || gate.run(|| next_ran.store(true, Ordering::SeqCst)))
    };
    wait_for(|| gate.queued() == 2);
    let (first, first_handle) = slots.remove(0);
    let _ = first.send(());
    first_handle.join().unwrap();
    wait_for(|| next_ran.load(Ordering::SeqCst));
    assert!(
        next_ran.load(Ordering::SeqCst),
        "the freed slot went to the next one"
    );
    let done = merging.join().unwrap();
    assert!(
        matches!(done.outcome, job::Outcome::Merge(Ok(None))),
        "no merge for a closed screen"
    );
    for (tx, handle) in slots {
        let _ = tx.send(());
        handle.join().unwrap();
    }
    next.join().unwrap();
    let e = cache.job_done(done, &host);
    assert!(e.is_empty());
    assert_eq!(cache.budget_bytes(), 0);
}

// --- aggregate pair cache ---------------------------------------------------------

#[test]
fn the_aggregate_is_evicted_as_a_pair_never_leaving_a_half_that_answers_the_ready_latch() {
    // A merged spectrogram costs ~20x its metrics per frame, so split budgets
    // always dropped the spectrogram first — and the ready latch keyed off
    // metrics alone, so the surviving half made the guard report "done" and
    // the missing half was never recomputed: no data, no loading state,
    // forever.
    let lane_a = lane_with(vec![clip("a1", 0), clip("a2", 2000)], "A");
    let lane_b = lane_with(vec![clip("b1", 0), clip("b2", 2000)], "B");
    let stub = Stub::new(&["a1.wav", "a2.wav", "b1.wav", "b2.wav"], 10);
    let mut host = Host::selecting(&lane_a);
    // Budget for roughly one lane's pair, so caching B evicts A.
    let mut cache = cache_with(&stub, budget(4000));
    let mut d = Driver::default();

    let e = cache.ensure_selected_spectrogram(&host);
    d.go(&mut cache, &host, e);
    assert!(cache.selected_metrics(&host).is_some());
    assert!(cache.selected_spectrogram(&host).is_some());

    host.select(&lane_b);
    let e = cache.ensure_selected_spectrogram(&host);
    d.go(&mut cache, &host, e);
    // Back to A: whatever eviction did, it did to BOTH halves — so the guard
    // sees a miss and recomputes, rather than serving a half.
    host.select(&lane_a);
    let e = cache.ensure_selected_spectrogram(&host);
    d.go(&mut cache, &host, e);
    assert!(cache.selected_metrics(&host).is_some());
    assert!(
        cache.selected_spectrogram(&host).is_some(),
        "a recomputed aggregate is whole"
    );
}

#[test]
fn a_refused_spectrogram_merge_still_serves_the_metrics_and_is_a_settled_answer() {
    // The spectrogram merge refuses lane spans past its cell budget where the
    // metrics merge succeeds. Half a pair is the honest result there; what
    // must not happen is the compute running again on every read. A lane
    // exactly at the span budget (300000 frames) at the production
    // spectrogram width (214 bins): 300000 x 214 cells is past the cell
    // budget while 300000 frames of metrics curves is not.
    let track = lane(vec![clip("c1", 0), clip("c2", 29_999_000)]);
    let stub = Stub::new(&["c1.wav", "c2.wav"], 10);
    for p in ["c1.wav", "c2.wav"] {
        stub.s().specs_by_path.insert(p.into(), spec(10, 214));
    }
    let host = Host::selecting(&track);
    let mut cache = cache(&stub);
    let mut d = Driver::default();

    let e = cache.ensure_selected_spectrogram(&host);
    d.go(&mut cache, &host, e);
    let calls_after_first = stub.s().both_calls;
    assert!(
        cache.selected_metrics(&host).is_some(),
        "metrics still serve"
    );
    assert!(
        cache.selected_spectrogram(&host).is_none(),
        "the merge refused it"
    );

    let e = cache.ensure_selected_spectrogram(&host);
    d.go(&mut cache, &host, e);
    assert_eq!(
        stub.s().both_calls,
        calls_after_first,
        "a settled pair is not recomputed on the next read"
    );
}

// --- a clip's analysis halves -----------------------------------------------------

fn single() -> (Arc<AudioTrack>, Arc<Stub>, Host) {
    let track = lane(vec![clip("c1", 0)]);
    let stub = Stub::new(&["c1.wav"], 10);
    let host = Host::selecting(&track);
    (track, stub, host)
}

#[test]
fn a_folded_out_lane_computes_the_pair_the_dock_reads_in_one_decode_with_its_ltas_and_profile() {
    // The Pitch lane (metrics) folded out first and the dock opened after:
    // two decodes, and the dock's summary built the LTAS and profile on the
    // UI thread (~0.4 s for a 10-minute take).
    let (track, stub, host) = single();
    let (m, sp) = {
        let s = stub.s();
        (
            s.metrics_by_path["c1.wav"].clone(),
            s.specs_by_path["c1.wav"].clone(),
        )
    };
    let ltas = Arc::new(compute_ltas(&sp, Some(&m)));
    let profile = Arc::new(analyze_voice_profile(&m, VoiceProfileOptions::default()));
    stub.s()
        .ltas_by_path
        .insert("c1.wav".into(), Arc::clone(&ltas));
    stub.s()
        .profile_by_path
        .insert("c1.wav".into(), Arc::clone(&profile));
    let mut cache = cache(&stub);
    let mut d = Driver::default();

    let e = cache.ensure_metrics_for(&track.id, &host);
    d.go(&mut cache, &host, e);
    let e = cache.ensure_selected_spectrogram(&host);
    d.go(&mut cache, &host, e);
    assert_eq!(stub.s().both_calls, 1);
    assert!(Arc::ptr_eq(&cache.selected_ltas(&host).unwrap(), &ltas));
    assert!(Arc::ptr_eq(
        &cache.selected_profile(&host).unwrap(),
        &profile
    ));

    // The spectrogram lane asks for the same pair, and joins it.
    let e = cache.ensure_spectrogram_for(&track.id, &host);
    d.go(&mut cache, &host, e);
    assert_eq!(stub.s().both_calls, 1);
}

#[test]
fn each_half_reads_as_computing_on_its_own() {
    // The metric tabs (pitch, vowel, timbre, vibrato, register) said the take
    // could not be analyzed while its metrics computed beside a held
    // spectrogram; read as one flag, the spectrogram views hid what they had
    // behind a spinner instead.
    let (_, stub, host) = single();
    let mut cache = cache(&stub);
    let mut d = Driver::default();
    let e = cache.ensure_selected_spectrogram(&host);
    d.go(&mut cache, &host, e);
    cache.debug_drop_half("c1.wav", true);
    assert!(cache.selected_analysis_incomplete(&host));

    d.hold_kinds.insert(JobKind::Metrics);
    let e = cache.ensure_selected_spectrogram(&host);
    d.go(&mut cache, &host, e);
    assert!(cache.selected_spectrogram(&host).is_some());
    assert!(cache.metrics_loading(&host));
    assert!(!cache.spectrogram_loading(&host));
    assert!(!cache.selected_analysis_incomplete(&host), "computing");

    d.release_kind(JobKind::Metrics);
    d.settle(&mut cache, &host);
    assert!(!cache.metrics_loading(&host));
    assert!(cache.selected_metrics(&host).is_some());
    assert!(!cache.selected_analysis_incomplete(&host));
}

#[test]
fn a_half_the_budget_dropped_reads_as_incomplete_until_asked_again() {
    let (_, stub, host) = single();
    let mut cache = cache(&stub);
    let mut d = Driver::default();
    let e = cache.ensure_selected_spectrogram(&host);
    d.go(&mut cache, &host, e);
    let e = cache.show_dock_tab(DockTab::Spectrogram, &host);
    d.go(&mut cache, &host, e);
    assert!(!cache.selected_analysis_incomplete(&host));
    cache.debug_drop_half("c1.wav", false);
    assert!(cache.selected_spectrogram(&host).is_none());
    assert!(cache.selected_analysis_incomplete(&host));
    let e = cache.ensure_selected_spectrogram(&host);
    d.go(&mut cache, &host, e);
    assert!(cache.selected_spectrogram(&host).is_some());
    assert!(!cache.selected_analysis_incomplete(&host));
}

#[test]
fn a_lane_asks_only_for_the_half_it_shows_once_the_other_is_in() {
    // Asking for the pair, a Pitch lane whose spectrogram the budget let go
    // decoded the take again for a spectrogram it never shows — after every
    // clip edit, which refreshes the folded-out lanes.
    let track = lane(vec![clip("c1", 0)]);
    let stub = Stub::new(&["c1.wav"], 10);
    let host = Host::with_tracks(&[&track]);
    let mut cache = cache(&stub);
    let mut d = Driver::default();
    let e = cache.ensure_metrics_for(&track.id, &host);
    d.go(&mut cache, &host, e);
    assert_eq!(stub.calls().0, 1, "neither half held: the pair");

    cache.debug_drop_half("c1.wav", false);
    let e = cache.ensure_metrics_for(&track.id, &host);
    d.go(&mut cache, &host, e);
    assert_eq!(stub.calls(), (1, 0, 0));

    let e = cache.ensure_spectrogram_for(&track.id, &host);
    d.go(&mut cache, &host, e);
    assert_eq!(stub.calls(), (1, 1, 0));

    cache.debug_drop_half("c1.wav", true);
    let e = cache.ensure_spectrogram_for(&track.id, &host);
    d.go(&mut cache, &host, e);
    assert_eq!(stub.calls(), (1, 1, 0));
    let e = cache.ensure_metrics_for(&track.id, &host);
    d.go(&mut cache, &host, e);
    assert_eq!(stub.calls(), (1, 1, 1));
}

#[test]
fn under_a_budget_that_cannot_hold_a_takes_pair_the_pair_stays_whole() {
    // Each half's insert dropped the other. The dock on the spectrogram and
    // the inspector on the metrics each asked again after the other's
    // compute, decoding the take back and forth; bounding that left one of
    // them blank, and closing the dock, a drag or a re-read pushing the shown
    // half out left it so for good.
    let track = lane(vec![clip("c1", 0)]);
    let other = lane_with(vec![clip("c2", 0)], "other");
    let stub = Stub::new(&["c1.wav", "c2.wav"], 10);
    let mut host = Host::selecting(&track);
    host.tracks.insert(other.id.clone(), Arc::clone(&other));
    let mut cache = cache_with(&stub, budget(1)); // less than any entry
    let mut d = Driver::default();
    let served = |cache: &AnalysisCache, host: &Host| {
        cache.selected_spectrogram(host).is_some() && cache.selected_metrics(host).is_some()
    };
    let any_view_asks = |cache: &AnalysisCache, host: &Host| {
        cache.lacks_analysis(false, true, host) || cache.selected_analysis_incomplete(host)
    };

    let e = cache.show_dock_tab(DockTab::Pitch, &host);
    d.go(&mut cache, &host, e);
    assert!(served(&cache, &host));
    assert_eq!(stub.decodes(), 1);
    for tab in [DockTab::Spectrogram, DockTab::Ltas, DockTab::Vowel] {
        let e = cache.show_dock_tab(tab, &host);
        d.go(&mut cache, &host, e);
        assert!(!any_view_asks(&cache, &host), "{tab:?}");
    }
    let e = cache.set_dock_open(false, &host);
    d.go(&mut cache, &host, e);
    let e = cache.ensure_selected_spectrogram(&host); // a drag's commit, a re-read
    d.go(&mut cache, &host, e);
    assert!(served(&cache, &host));
    assert_eq!(stub.decodes(), 1, "no back and forth");

    // Another take's analysis pushes this one out: asked again, not left
    // blank.
    let e = cache.ensure_metrics_for("other", &host);
    d.go(&mut cache, &host, e);
    assert!(cache.lacks_analysis(false, true, &host));
    let e = cache.heal_selected_analysis(Some(false), Some(true), &host);
    d.go(&mut cache, &host, e);
    assert!(cache.selected_metrics(&host).is_some());
}

#[test]
fn a_half_pushed_out_while_a_heal_computes_is_asked_for_again() {
    let (_, stub, host) = single();
    let mut cache = cache(&stub);
    let mut d = Driver::default();
    let e = cache.ensure_selected_spectrogram(&host);
    d.go(&mut cache, &host, e);
    let e = cache.show_dock_tab(DockTab::Spectrogram, &host);
    d.go(&mut cache, &host, e);
    cache.debug_drop_half("c1.wav", false);
    // Pressure while the heal computes: the metrics go meanwhile.
    d.hold_kinds.insert(JobKind::Spectrogram);
    let e = cache.heal_selected_analysis(None, None, &host);
    d.go(&mut cache, &host, e);
    cache.debug_drop_half("c1.wav", true);
    d.release_kind(JobKind::Spectrogram);
    d.settle(&mut cache, &host);
    assert!(cache.selected_spectrogram(&host).is_some());
    assert!(cache.lacks_analysis(false, true, &host));
    let e = cache.heal_selected_analysis(Some(false), Some(true), &host);
    d.go(&mut cache, &host, e);
    assert!(cache.selected_metrics(&host).is_some());
}

#[test]
fn a_heal_that_made_the_take_whole_leaves_the_next_one_free() {
    let (_, stub, host) = single();
    let mut cache = cache(&stub);
    let mut d = Driver::default();
    let e = cache.ensure_selected_spectrogram(&host);
    d.go(&mut cache, &host, e);
    let e = cache.show_dock_tab(DockTab::Spectrogram, &host);
    d.go(&mut cache, &host, e);
    for i in 1..=2 {
        cache.debug_drop_half("c1.wav", false);
        assert!(cache.selected_analysis_incomplete(&host), "drop {i}");
        let e = cache.heal_selected_analysis(None, None, &host);
        d.go(&mut cache, &host, e);
        assert!(cache.selected_spectrogram(&host).is_some(), "heal {i}");
    }
    assert_eq!(stub.calls().1, 2);
}

// --- shared analysis budget -------------------------------------------------------

#[test]
fn derived_results_never_outlive_the_source_entry_they_came_from() {
    // profile / LTAS / register are memoized under the SAME key as the
    // analysis they derive from, in plain maps. A budget eviction can drop
    // that source on any insert, and the derived value keyed off it then had
    // no owner left to drop it.
    let (_, stub, host) = single();
    let mut cache = cache(&stub);
    let mut d = Driver::default();
    let e = cache.ensure_selected_spectrogram(&host);
    d.go(&mut cache, &host, e);
    // Materialize the derived memos.
    let _ = cache.selected_profile(&host);
    let _ = cache.selected_ltas(&host);
    let _ = cache.selected_register(&host);
    assert!(cache.derived_entries() > 0);

    cache.evict_analysis("c1.wav", false);

    assert_eq!(
        cache.derived_entries(),
        0,
        "the derived memos go with their source"
    );
}

#[test]
fn a_clip_pairs_ltas_and_profile_are_served_from_the_analysis_compute() {
    let (_, stub, host) = single();
    let (m, sp) = {
        let s = stub.s();
        (
            s.metrics_by_path["c1.wav"].clone(),
            s.specs_by_path["c1.wav"].clone(),
        )
    };
    let ltas = Arc::new(compute_ltas(&sp, Some(&m)));
    let profile = Arc::new(analyze_voice_profile(&m, VoiceProfileOptions::default()));
    stub.s()
        .ltas_by_path
        .insert("c1.wav".into(), Arc::clone(&ltas));
    stub.s()
        .profile_by_path
        .insert("c1.wav".into(), Arc::clone(&profile));
    let mut cache = cache(&stub);
    let mut d = Driver::default();

    let e = cache.ensure_selected_spectrogram(&host);
    d.go(&mut cache, &host, e);
    assert_eq!(stub.s().both_calls, 1);
    assert!(Arc::ptr_eq(&cache.selected_ltas(&host).unwrap(), &ltas));
    assert!(Arc::ptr_eq(
        &cache.selected_profile(&host).unwrap(),
        &profile
    ));
    // ...and they go with their source, like any derived memo.
    cache.evict_analysis("c1.wav", false);
    assert_eq!(cache.derived_entries(), 0);
}

#[test]
fn a_disposed_screen_stops_charging_the_shared_budget() {
    // The budget outlives any one screen, so a cache that does not leave it
    // keeps that screen's whole payload resident for the process's life.
    let shared = LruBytesBudget::new(1 << 30);
    let (_, stub, host) = single();
    let mut cache = cache_with(
        &stub,
        AnalysisCacheOptions {
            cache_budget: Some(shared.clone()),
            ..AnalysisCacheOptions::default()
        },
    );
    let mut d = Driver::default();
    let e = cache.ensure_selected_spectrogram(&host);
    d.go(&mut cache, &host, e);
    assert!(shared.bytes() > 0);

    cache.dispose();

    assert_eq!(shared.bytes(), 0);
    assert_eq!(shared.member_count(), 0);
}

// --- dispose abandons in-flight analysis ------------------------------------------

#[test]
fn a_disposed_cache_admits_no_further_clip_computes() {
    // Heavy computes queue through the app-wide analysis gate's two slots, so
    // an aggregate that keeps feeding clips in after its screen closed makes
    // the NEXT screen wait behind results no one will read.
    let track = lane(vec![clip("c1", 0), clip("c2", 2000), clip("c3", 4000)]);
    let stub = Stub::new(&["c1.wav", "c2.wav", "c3.wav"], 10);
    let host = Host::selecting(&track);
    let mut cache = cache(&stub);
    let mut d = Driver::default();
    // Dispose the moment the FIRST clip's compute lands.
    d.hold("c1.wav");
    let e = cache.ensure_selected_spectrogram(&host);
    d.go(&mut cache, &host, e);
    cache.dispose();
    d.release("c1.wav");
    d.settle(&mut cache, &host);

    assert_eq!(
        stub.s().both_calls,
        1,
        "clips after the dispose must never be admitted"
    );
}

#[test]
fn a_compute_already_in_flight_does_not_write_into_a_disposed_cache() {
    let (_, stub, host) = single();
    let mut cache = cache(&stub);
    let mut d = Driver::default();
    d.hold("c1.wav");
    let e = cache.ensure_selected_spectrogram(&host);
    d.go(&mut cache, &host, e);
    cache.dispose();
    d.release("c1.wav");
    d.settle(&mut cache, &host);

    assert_eq!(cache.budget_bytes(), 0, "nothing was cached after dispose");
}

#[test]
fn a_lane_merge_asked_for_after_a_reanalyze_eviction_while_the_stale_compute_is_in_flight_is_not_left_blank(
) {
    // The merge joined the superseded compute and read its hole; the rerun
    // started only after, and nothing asked for the lane again.
    let lane_c = lane_with(vec![clip("c1", 0), clip("c2", 2000)], "C");
    let stub = Stub::new(&["c1.wav", "c2.wav"], 10);
    let mut host = Host::selecting(&lane_c);
    host.clip_id = Some("c2".into());
    let mut cache = cache(&stub);
    let mut d = Driver::default();
    d.hold("c2.wav");
    // The dock analyzes the selected clip c2 (a long file: still computing).
    let e = cache.ensure_selected_spectrogram(&host);
    d.go(&mut cache, &host, e);
    // Re-analyze lane C: each clip evicted for recompute.
    cache.evict_analysis("c1.wav", true);
    cache.evict_analysis("c2.wav", true);
    stub.s()
        .metrics_by_path
        .insert("c2.wav".into(), metrics(20));
    // The lane is folded out (or re-analyze's on_track_audio_changed asks for
    // it) while c2's superseded compute is still running.
    let e = cache.toggle_track_expanded("C", &host);
    d.go(&mut cache, &host, e);
    d.release("c2.wav");
    d.settle(&mut cache, &host);
    assert!(cache.metrics_for("C", &host).is_some());
}

// --- From playhead_repaint_test.dart (the cache's side) ----------------------------

#[test]
fn the_dock_asks_again_for_a_spectrogram_the_cache_let_go() {
    // The two halves of a clip's analysis are separate cache entries, and
    // under the byte budget's pressure the spectrogram went first: nothing
    // asked for it again, and the dock said the take could not be analyzed.
    let (_, stub, host) = single();
    let mut cache = cache(&stub);
    let mut d = Driver::default();
    let e = cache.show_dock_tab(DockTab::Spectrogram, &host);
    d.go(&mut cache, &host, e);
    assert!(cache.selected_spectrogram(&host).is_some());

    cache.debug_drop_half("c1.wav", false);
    // Any rebuild of the dock asks.
    assert!(cache.selected_analysis_incomplete(&host));
    let e = cache.heal_selected_analysis(None, None, &host);
    d.go(&mut cache, &host, e);
    assert!(cache.selected_spectrogram(&host).is_some());
}

#[test]
fn dragging_a_clip_leaves_the_lane_merge_waiting_for_where_it_lands() {
    // Each tick re-keys the lane's merge, and nothing asks for it meanwhile;
    // its views wait for the merge where it lands rather than say the take
    // cannot be analyzed.
    let at = |ms: i64| {
        lane_with(
            vec![
                AudioClip {
                    start_ms: 0,
                    ..AudioClip::new("c0", "t0.wav", 10_000)
                },
                AudioClip {
                    start_ms: ms,
                    ..AudioClip::new("c1", "t0b.wav", 10_000)
                },
            ],
            "t0",
        )
    };
    let stub = Stub::new(&["t0.wav", "t0b.wav"], 100);
    let mut host = Host::selecting(&at(20_000));
    let mut cache = cache(&stub);
    let mut d = Driver::default();
    let e = cache.toggle_track_expanded("t0", &host); // the pitch lane
    d.go(&mut cache, &host, e);
    // Selected with no clip: the inspector and the dock show the lane's merge
    // too, and ask for it on their own.
    let e = cache.show_dock_tab(DockTab::Spectrogram, &host);
    d.go(&mut cache, &host, e);
    assert!(cache.metrics_for("t0", &host).is_some());

    host.moving.insert("t0".into());
    for ms in (20_100..=20_500).step_by(100) {
        host.select(&at(ms));
        // What the views ask while they rebuild.
        assert!(!cache.lane_lacks_data("t0", &host));
        assert!(!cache.selected_analysis_incomplete(&host));
    }
    assert!(cache.metrics_for("t0", &host).is_none(), "nothing merged");
    // The take's own lanes, the dock and the inspector wait for the merge
    // where it lands too.
    assert!(cache.metrics_loading_for("t0", &host));
    assert!(cache.spectrogram_loading_for("t0", &host));
    assert!(cache.spectrogram_loading(&host));
    assert!(cache.metrics_loading(&host));

    // The move commits; the view model asks for where it landed.
    host.moving.clear();
    let e = cache.ensure_selected_spectrogram(&host);
    d.go(&mut cache, &host, e);
    let e = cache.refresh_expanded_lanes(&host);
    d.go(&mut cache, &host, e);
    assert!(cache.metrics_for("t0", &host).is_some(), "where it landed");
    assert!(!cache.spectrogram_loading(&host));
}

// --- The EQ display spectrogram ----------------------------------------------------

#[test]
fn the_eq_column_follows_the_clip_under_the_playhead_and_computes_its_display_spectrogram() {
    let track = lane(vec![clip("c1", 0), clip("c2", 2000)]);
    let stub = Stub::new(&["c1.wav", "c2.wav"], 10);
    let mut host = Host::with_tracks(&[&track]);
    host.position = Duration::from_millis(2500);
    let mut cache = cache(&stub);
    let mut d = Driver::default();

    let (column, e) = cache.eq_input_column("lane", &host);
    assert!(column.is_none(), "not computed yet");
    assert_eq!(
        e.iter()
            .map(|e| match e {
                AnalysisEffect::Run(job) => (job.kind(), job.path().map(str::to_string)),
                other => panic!("{other:?}"),
            })
            .collect::<Vec<_>>(),
        vec![(JobKind::EqSpectrogram, Some("c2.wav".to_string()))]
    );
    d.go(&mut cache, &host, e);
    let (column, e) = cache.eq_input_column("lane", &host);
    assert!(e.is_empty());
    let column = column.expect("computed");
    assert_eq!(column.col().len(), 2);
    assert_eq!(column.bin_hz(), SR as f64 / 2048.0);

    // Between clips there is nothing to show.
    host.position = Duration::from_millis(1500);
    assert!(cache.eq_input_column("lane", &host).0.is_none());
}

// --- The view-model half of spectrogram_wanted_test.dart --------------------------

#[test]
fn a_job_carries_its_wanted_predicate_to_the_worker_never_the_cache() {
    // The predicate closes over the cache, which must not ride to the
    // worker: the job is Send, and its predicate is atomics the cache flips.
    fn send<T: Send + 'static>() {}
    send::<AnalysisJob>();
    send::<JobDone>();
    send::<AnalysisCache>();

    let (_, stub, host) = single();
    let mut cache = cache(&stub);
    let mut d = Driver::default();
    d.hold("c1.wav");
    let e = cache.ensure_selected_spectrogram(&host);
    let job = match &e[..] {
        [AnalysisEffect::Run(job)] => job,
        other => panic!("{other:?}"),
    };
    assert!(job.is_wanted());
    let probe = job.wanted_probe();
    let wanted_on_worker = thread::spawn(move || probe.is_wanted()).join().unwrap();
    assert!(wanted_on_worker);
    d.go(&mut cache, &host, e);
    cache.dispose();
    assert!(!d.last_wanted.unwrap().is_wanted());
}

#[test]
fn a_failed_compute_latches_unavailable_and_is_not_retried() {
    // Dart's `catch (_)`: a compute that threw is latched like one that found
    // nothing.
    struct Failing;
    impl SpectrogramAnalyses for Failing {
        fn compute_from_file(
            &self,
            _: &str,
            _: AnalysisParams,
            _: Option<&dyn Fn() -> bool>,
        ) -> io::Result<Option<Arc<Spectrogram>>> {
            Err(io::Error::other("unreadable"))
        }
        fn compute_metrics_from_file(
            &self,
            _: &str,
            _: AnalysisParams,
            _: Option<&dyn Fn() -> bool>,
        ) -> io::Result<Option<Arc<VoiceMetrics>>> {
            panic!("a compute that panics")
        }
        fn compute_both_from_file(
            &self,
            _: &str,
            _: AnalysisParams,
            _: Option<&dyn Fn() -> bool>,
        ) -> io::Result<Option<TakeAnalysisBundle>> {
            Err(io::Error::other("unreadable"))
        }
    }
    let track = lane(vec![clip("c1", 0)]);
    let host = Host::selecting(&track);
    let mut cache = AnalysisCache::with_options(
        Arc::new(Failing),
        Arc::new(MonitorService::default()),
        None,
        AnalysisCacheOptions {
            cache_budget: Some(LruBytesBudget::new(1 << 30)),
            ..AnalysisCacheOptions::default()
        },
    );
    let mut d = Driver::default();
    let e = cache.ensure_selected_spectrogram(&host);
    d.go(&mut cache, &host, e);
    assert!(cache.selected_metrics_unavailable(&host));
    assert!(!cache.spectrogram_loading(&host));
    assert!(!cache.metrics_loading(&host));
    let e = cache.ensure_selected_spectrogram(&host);
    assert!(e.is_empty(), "latched: not retried");
}
