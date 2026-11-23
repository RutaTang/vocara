//! The analysis cache's off-UI-thread work: the per-clip computes (Dart's
//! `computeFromFile` / `computeMetricsFromFile` / `computeBothFromFile` calls,
//! each its own isolate inside the service) and the lane merge (Dart's
//! `_mergeLaneInGate`: `Isolate.run` inside an analysis-gate slot).
//!
//! An [`AnalysisJob`] carries everything its worker needs and nothing of the
//! cache: the service, the path, and a [`WantedProbe`] — shared atomics the
//! cache flips on the UI thread (its dispose, a path's generation, a lane
//! merge disowned by an eviction) and the worker reads once the gate admits
//! the job. That is Dart's rule that the `wanted` predicate must not ride
//! into the isolate, in Rust's terms: the job is `Send`, the cache is not
//! reachable from it.

use std::fmt;
use std::io;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use vocara_model::models::spectrogram::{merge_spectrograms, Spectrogram, SpectrogramPart};
use vocara_model::models::voice_metrics::{
    analyze_voice_profile, compute_ltas, merge_voice_metrics, Ltas, VoiceMetrics, VoiceMetricsPart,
    VoiceProfile, VoiceProfileOptions,
};
use vocara_model::util::analysis_gate::AnalysisGate;
use vocara_services::audio_dsp::AnalysisParams;
use vocara_services::spectrogram_service::{SpectrogramAnalyses, TakeAnalysisBundle};

use crate::util::panic_message;

/// What an [`AnalysisJob`] computes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JobKind {
    /// A clip's analysis spectrogram alone (5 kHz).
    Spectrogram,
    /// A clip's voice metrics alone.
    Metrics,
    /// A clip's spectrogram and metrics from one decode, with the pair's LTAS
    /// and voice profile.
    Both,
    /// A clip's wide-range (18 kHz) display spectrogram for the FX EQ.
    EqSpectrogram,
    /// A multi-clip lane's merged analysis, in an analysis-gate slot.
    MergeLane,
}

/// Whether a job is still wanted once the gate admits it (Dart's `wanted`
/// closures): the cache is still open and, for a clip compute, its file is
/// unchanged; for a lane merge, no eviction disowned it. Cheap to clone and
/// `Send`: atomics the cache shares with its jobs.
#[derive(Clone)]
pub struct WantedProbe {
    disposed: Arc<AtomicBool>,
    test: WantedTest,
}

#[derive(Clone)]
enum WantedTest {
    /// The path's generation is still the one the compute started under.
    Generation { cell: Arc<AtomicU64>, at: u64 },
    /// The lane merge has not been disowned.
    NotDisowned(Arc<AtomicBool>),
}

impl WantedProbe {
    pub(super) fn generation(disposed: Arc<AtomicBool>, cell: Arc<AtomicU64>, at: u64) -> Self {
        WantedProbe {
            disposed,
            test: WantedTest::Generation { cell, at },
        }
    }

    pub(super) fn not_disowned(disposed: Arc<AtomicBool>, disowned: Arc<AtomicBool>) -> Self {
        WantedProbe {
            disposed,
            test: WantedTest::NotDisowned(disowned),
        }
    }

    /// The predicate itself.
    pub fn is_wanted(&self) -> bool {
        if self.disposed.load(Ordering::SeqCst) {
            return false;
        }
        match &self.test {
            WantedTest::Generation { cell, at } => cell.load(Ordering::SeqCst) == *at,
            WantedTest::NotDisowned(disowned) => !disowned.load(Ordering::SeqCst),
        }
    }
}

impl fmt::Debug for WantedProbe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WantedProbe")
            .field("wanted", &self.is_wanted())
            .finish()
    }
}

/// A lane's merged analysis plus the LTAS and voice profile the dock derives
/// from it (Dart's `_MergedLane`): exactly what the getters would build
/// (`selected_ltas` needs the spectrogram, `selected_profile` the metrics).
#[derive(Clone, Debug, Default)]
pub struct MergedLane {
    pub metrics: Option<Arc<VoiceMetrics>>,
    pub spectrogram: Option<Arc<Spectrogram>>,
    pub ltas: Option<Arc<Ltas>>,
    pub profile: Option<Arc<VoiceProfile>>,
}

pub(super) enum Work {
    Compute {
        kind: JobKind,
        service: Arc<dyn SpectrogramAnalyses>,
        path: String,
        params: AnalysisParams,
        wanted: WantedProbe,
    },
    Merge {
        gate: &'static AnalysisGate,
        metrics: Vec<VoiceMetricsPart>,
        specs: Vec<SpectrogramPart>,
        wanted: WantedProbe,
    },
}

/// One piece of blocking analysis work: run it on a worker thread
/// ([`crate::util::blocking`]) and hand its [`JobDone`] back to
/// `AnalysisCache::job_done`.
pub struct AnalysisJob {
    pub(super) id: u64,
    pub(super) work: Work,
}

impl AnalysisJob {
    pub fn kind(&self) -> JobKind {
        match &self.work {
            Work::Compute { kind, .. } => *kind,
            Work::Merge { .. } => JobKind::MergeLane,
        }
    }

    /// The clip file a compute reads (`None` for a lane merge).
    pub fn path(&self) -> Option<&str> {
        match &self.work {
            Work::Compute { path, .. } => Some(path),
            Work::Merge { .. } => None,
        }
    }

    /// Whether the job is still wanted (what it asks once the gate admits
    /// it).
    pub fn is_wanted(&self) -> bool {
        self.wanted_probe().is_wanted()
    }

    /// The job's `wanted` predicate, to keep asking after the job is gone to
    /// its worker.
    pub fn wanted_probe(&self) -> WantedProbe {
        match &self.work {
            Work::Compute { wanted, .. } | Work::Merge { wanted, .. } => wanted.clone(),
        }
    }

    /// Runs the job, blocking: the clip computes queue at the app-wide
    /// analysis gate inside the service, the lane merge at the cache's gate
    /// here. Never panics: a failure (an I/O error, a panic in the compute)
    /// comes back as the [`JobDone`]'s error, Dart's thrown compute.
    pub fn run(self) -> JobDone {
        let id = self.id;
        let outcome = match self.work {
            Work::Compute {
                kind,
                service,
                path,
                params,
                wanted,
            } => {
                let predicate = move || wanted.is_wanted();
                let wanted: Option<&dyn Fn() -> bool> = Some(&predicate);
                match kind {
                    JobKind::Metrics => Outcome::Metrics(caught(|| {
                        service.compute_metrics_from_file(&path, params, wanted)
                    })),
                    JobKind::Both => Outcome::Both(caught(|| {
                        service.compute_both_from_file(&path, params, wanted)
                    })),
                    _ => Outcome::Spectrogram(caught(|| {
                        service.compute_from_file(&path, params, wanted)
                    })),
                }
            }
            Work::Merge {
                gate,
                metrics,
                specs,
                wanted,
            } => Outcome::Merge(caught(|| {
                // Holding one of the app-wide analysis slots like every other
                // heavy compute: its input is every clip's analysis. Nothing,
                // without a merge, when it is no longer wanted once a slot
                // admits it.
                Ok(gate.run(|| {
                    if wanted.is_wanted() {
                        Some(merge_lane(&metrics, &specs))
                    } else {
                        None
                    }
                }))
            })),
        };
        JobDone { id, outcome }
    }
}

impl fmt::Debug for AnalysisJob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AnalysisJob")
            .field("id", &self.id)
            .field("kind", &self.kind())
            .field("path", &self.path())
            .finish()
    }
}

/// What a job produced; `Err` is the message of what failed (Dart's caught
/// throw).
#[derive(Clone, Debug)]
pub(super) enum Outcome {
    Spectrogram(Result<Option<Arc<Spectrogram>>, String>),
    Metrics(Result<Option<Arc<VoiceMetrics>>, String>),
    Both(Result<Option<TakeAnalysisBundle>, String>),
    Merge(Result<Option<MergedLane>, String>),
}

/// A finished [`AnalysisJob`], for `AnalysisCache::job_done`.
#[derive(Clone, Debug)]
pub struct JobDone {
    pub(super) id: u64,
    pub(super) outcome: Outcome,
}

/// Runs `f`, turning an error or a panic into its message.
fn caught<T>(f: impl FnOnce() -> io::Result<T>) -> Result<T, String> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(e)) => Err(e.to_string()),
        Err(payload) => Err(panic_message(payload.as_ref())),
    }
}

/// A lane's merged analysis plus the LTAS and voice profile the dock derives
/// from it, exactly as its getters would. Pure, so it runs on a worker.
pub(super) fn merge_lane(metrics: &[VoiceMetricsPart], specs: &[SpectrogramPart]) -> MergedLane {
    let m = merge_voice_metrics(metrics);
    let spec = merge_spectrograms(specs);
    MergedLane {
        ltas: spec
            .as_ref()
            .map(|s| Arc::new(compute_ltas(s, m.as_deref()))),
        profile: m
            .as_ref()
            .map(|m| Arc::new(analyze_voice_profile(m, VoiceProfileOptions::default()))),
        metrics: m,
        spectrogram: spec,
    }
}
