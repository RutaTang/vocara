//! A complete offline analysis of one clip (port of
//! `lib/domain/models/technique_analysis.dart`).

use std::sync::Arc;

use super::spectrogram::Spectrogram;
use super::voice_metrics::{Ltas, RegisterMap, VoiceMetrics, VoiceProfile};

/// A complete offline analysis of one audio clip — the same suite a vocal take
/// gets (per-frame timbre metrics, spectrogram, long-term average spectrum,
/// voice profile and register map).
///
/// Used two ways in the technique library: as a technique's reference "ground
/// truth" (computed from its example media), and for an ephemeral practice
/// recording compared against that reference. Immutable; produced by
/// `MediaAnalysisService`.
#[derive(Clone, Debug)]
pub struct TechniqueAnalysis {
    /// Per-frame timbre metrics (F0/H1−H2/ring/tilt/CPP/formants).
    pub metrics: Arc<VoiceMetrics>,
    /// Time–frequency magnitude image.
    pub spectrogram: Arc<Spectrogram>,
    /// Long-term average spectrum (the clip's overall spectral shape).
    pub ltas: Arc<Ltas>,
    /// Range / passaggio / dynamics summary derived from [metrics].
    pub profile: Arc<VoiceProfile>,
    /// Chest / mix / head register classification over the clip.
    pub register: Arc<RegisterMap>,
    /// Clip length in milliseconds.
    pub duration_ms: i64,
}

// Field-wise value equality. Note that the data-heavy members
// (VoiceMetrics, Spectrogram, Ltas, VoiceProfile, RegisterMap) deliberately
// keep IDENTITY equality — comparing their big Float32 buffers element-wise
// would be a performance trap — so two analyses compare equal only when they
// share the same analysis artifacts.
impl PartialEq for TechniqueAnalysis {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.metrics, &other.metrics)
            && Arc::ptr_eq(&self.spectrogram, &other.spectrogram)
            && Arc::ptr_eq(&self.ltas, &other.ltas)
            && Arc::ptr_eq(&self.profile, &other.profile)
            && Arc::ptr_eq(&self.register, &other.register)
            && self.duration_ms == other.duration_ms
    }
}
