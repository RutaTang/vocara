//! Spectrograms and voice metrics of WAV takes (port of
//! `lib/data/services/spectrogram_service.dart`).
//!
//! Dart probed the header on the UI isolate, queued at the app-wide gate
//! there, and ran each compute in an `Isolate.run` that got only the path.
//! Here every `compute_*` is one blocking call for a worker thread
//! (`spawn_blocking`): the existence check, the header-only admission, the
//! wait at [`analysis_gate`] and the windowed compute all run on that thread,
//! in that order. The `wanted` predicate is only borrowed and called there,
//! so it never has to be `Send` (the Rust shape of Dart's rule that the
//! isolate closure must not capture the caller).

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

use vocara_model::models::spectrogram::Spectrogram;
use vocara_model::models::voice_metrics::{
    analyze_voice_profile, compute_ltas, Ltas, VoiceMetrics, VoiceProfile, VoiceProfileOptions,
};
use vocara_model::util::analysis_gate::analysis_gate;

use crate::audio_dsp::{
    stft_frame_count, AnalysisParams, PcmAudio, StreamedTakeAnalysis, StreamingSpectrogramBuilder,
    StreamingTakeAnalyzer, MAX_ANALYSIS_SAMPLE_RATE,
};
use crate::engine::media::cap_frames;
use crate::media_analysis_service::MAX_ANALYSIS_DURATION;

/// The spectrogram and the voice metrics computed from ONE decode of the same
/// file (see [`SpectrogramService::compute_both_from_file`]), plus the views
/// derived from exactly that pair, computed on the analysis thread beside it:
/// [ltas] is `compute_ltas(spectrogram, metrics)` and [profile]
/// `analyze_voice_profile(metrics)`. Built on the UI thread instead, on first
/// display, they cost a 10-minute take ~250 ms of frame time (12M `pow` calls
/// for the LTAS alone). `None` where their source is `None`.
#[derive(Clone, Debug, Default)]
pub struct TakeAnalysisBundle {
    pub spectrogram: Option<Arc<Spectrogram>>,
    pub metrics: Option<Arc<VoiceMetrics>>,
    pub ltas: Option<Arc<Ltas>>,
    pub profile: Option<Arc<VoiceProfile>>,
}

/// Everything the admission gate learned from the header, handed to the
/// streaming reader so the compute never re-walks (or re-trusts) the chunks:
/// where the PCM lives, its layout, and the effective sample format
/// (1 = integer PCM, 3 = IEEE float — extensible resolved to its SubFormat).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WavAdmission {
    /// Byte offset of the first PCM byte.
    pub data_start: u64,
    /// Whole frames the file physically holds within the data chunk.
    pub frames: usize,
    pub sample_rate: i64,
    pub channels: usize,
    pub bits: usize,
    pub format: u16,
}

/// Absolute ceiling on admitted frames, INDEPENDENT of the header's
/// arithmetic — a forged header must not be able to talk its way past it.
/// Derivation: the legal upper bound the analysis is designed for is a
/// 10-minute 48 kHz stereo float32 WAV ([`MAX_ANALYSIS_DURATION`]), i.e.
/// 10*60*48000 = 28.8 M frames and ~230 MB of PCM. Frames matter separately
/// from bytes because the decoder expands to f32 MONO (frames * 4 bytes): a
/// large 8-bit mono file is small on disk but 4x bigger decoded, so a byte
/// ceiling alone is not a memory bound.
pub const MAX_ANALYSIS_FRAMES: u64 = 30 * 1000 * 1000;

/// Absolute ceiling on the admitted file's bytes up to the data chunk's end
/// (see [`MAX_ANALYSIS_FRAMES`]).
pub const MAX_ANALYSIS_BYTES: u64 = 240 * 1000 * 1000;

/// File bytes per decode chunk of [`read_wav_mono_chunked`] (4 MiB).
const CHUNK_BUDGET: usize = 4 << 20;

/// What the song screens need from the spectrogram service (Dart tests faked
/// this class by overriding these methods). Every method blocks: run it on a
/// worker thread.
pub trait SpectrogramAnalyses: Send + Sync {
    /// See [`SpectrogramService::compute_from_file`].
    fn compute_from_file(
        &self,
        path: &str,
        params: AnalysisParams,
        wanted: Option<&dyn Fn() -> bool>,
    ) -> io::Result<Option<Arc<Spectrogram>>>;

    /// See [`SpectrogramService::compute_metrics_from_file`].
    fn compute_metrics_from_file(
        &self,
        path: &str,
        params: AnalysisParams,
        wanted: Option<&dyn Fn() -> bool>,
    ) -> io::Result<Option<Arc<VoiceMetrics>>>;

    /// See [`SpectrogramService::compute_both_from_file`].
    fn compute_both_from_file(
        &self,
        path: &str,
        params: AnalysisParams,
        wanted: Option<&dyn Fn() -> bool>,
    ) -> io::Result<Option<TakeAnalysisBundle>>;
}

/// Computes spectrograms and voice metrics from audio files off the UI
/// thread. v1 supports uncompressed WAV (our recordings + dropped WAV);
/// returns `None` for other formats.
///
/// Memory discipline: the file is streamed in bounded chunks straight into
/// the analysis, so neither the raw bytes nor the whole-track samples ever
/// exist at once. Files longer than [`MAX_ANALYSIS_DURATION`] — and files
/// that are not WAV at all — are refused by a header-only probe BEFORE
/// anything allocates: the old behavior loaded the entire file just to have
/// the decoder say no.
#[derive(Clone, Copy, Debug, Default)]
pub struct SpectrogramService;

/// Reads up to [n] bytes at the file's position: fewer only at its end
/// (Dart's `RandomAccessFile.read`).
fn read_up_to(file: &mut File, n: usize) -> io::Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(n);
    file.by_ref().take(n as u64).read_to_end(&mut buf)?;
    Ok(buf)
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// Header-only admission gate for the analysis paths: walks the WAV chunk
/// list (seeking over chunk bodies, so a multi-gigabyte file costs a few
/// hundred bytes of IO) and answers "should this file be analyzed, and how
/// many bytes of it?". Returns `None` (skip) when:
///  * the file is not RIFF/WAVE — the WAV-only decoder would fail AFTER the
///    whole file loaded;
///  * no fmt/data chunk can be found — same reasoning;
///  * the declared PCM duration exceeds the analysis cap (a streamed WAV's
///    0xFFFFFFFF, no size, declares the rest of the file);
///  * the file cannot be read.
///
/// Otherwise returns where the PCM lives and how many whole frames of it to
/// read: the data chunk's end bounds the read, so a small declared data
/// chunk with gigabytes of trailing junk (appended metadata, garbage, a
/// lying tail) can no longer ride into memory on the strength of an honest
/// header. Handles WAVE_FORMAT_EXTENSIBLE and extra chunks (LIST etc.),
/// unlike the canonical-44-byte take probe — an imported WAV is exactly the
/// file most likely to carry them.
pub fn admit_for_analysis(path: &Path) -> Option<WavAdmission> {
    // Unreadable now → unreadable in the compute too; don't load it there.
    admit(path).ok().flatten()
}

fn admit(path: &Path) -> io::Result<Option<WavAdmission>> {
    let mut raf = File::open(path)?;
    let head = read_up_to(&mut raf, 12)?;
    if head.len() < 12 {
        return Ok(None);
    }
    if &head[0..4] != b"RIFF" || &head[8..12] != b"WAVE" {
        return Ok(None);
    }
    let mut offset: u64 = 12;
    let (mut sample_rate, mut block_align, mut channels, mut bits, mut format) =
        (0u32, 0u16, 0u16, 0u16, 0u16);
    // Bounded walk: a well-formed WAV reaches `data` within a handful of
    // chunks; 64 tolerates every real-world metadata pile-up while a
    // malformed chain can't spin forever.
    for _ in 0..64 {
        raf.seek(SeekFrom::Start(offset))?;
        let ch = read_up_to(&mut raf, 8)?;
        if ch.len() < 8 {
            return Ok(None); // ran out before a data chunk
        }
        let id = &ch[0..4];
        let size = u32_at(&ch, 4);
        if id == b"fmt " {
            let body = read_up_to(&mut raf, size.min(40) as usize)?;
            if body.len() < 16 {
                return Ok(None);
            }
            sample_rate = u32_at(&body, 4);
            block_align = u16_at(&body, 12);
            // The gate's frame count comes from size / block_align, so a
            // FORGED block_align is exactly how a huge file talks its frame
            // count down under the duration cap. Same admission rules as the
            // edit service's parser: an uncompressed block_align is exactly
            // channels * ceil(bits / 8), and the supported envelope is bounded
            // (channels, bit depth, rate) so no header arithmetic can inflate
            // the ceilings below.
            channels = u16_at(&body, 2);
            bits = u16_at(&body, 14);
            // Effective sample format: WAVE_FORMAT_EXTENSIBLE (0xFFFE)
            // resolves to the SubFormat GUID's leading code at fmt offset 24.
            let tag = u16_at(&body, 0);
            format = if tag == 0xFFFE && body.len() >= 26 {
                u16_at(&body, 24)
            } else {
                tag
            };
            // Integer PCM64 does not exist in the wild and the reference
            // decoder has no branch for it — admitting it would decode a
            // whole track of silence (the reviewed regression). Float is
            // 32/64.
            let legal_format = match format {
                1 => matches!(bits, 8 | 16 | 24 | 32),
                3 => bits == 32 || bits == 64,
                _ => false,
            };
            // The rate ceiling is the ANALYSIS envelope, not a header-sanity
            // bound: above MAX_ANALYSIS_SAMPLE_RATE the YIN frame can no
            // longer hold the fmin search lag, so every voice metric comes
            // back empty while the spectrogram still renders (see the
            // constant's doc). Admitting was the worse failure — a
            // healthy-looking take whose analyses are silently blank.
            if !(1..=8).contains(&channels)
                || !legal_format
                || sample_rate < 8000
                || i64::from(sample_rate) > MAX_ANALYSIS_SAMPLE_RATE
                || u32::from(block_align) != u32::from(channels) * u32::from(bits.div_ceil(8))
            {
                return Ok(None);
            }
        } else if id == b"data" {
            if sample_rate == 0 || block_align == 0 || format == 0 {
                return Ok(None);
            }
            // 0xFFFFFFFF declares no size: a WAV written as a stream (to a
            // pipe, or by a recorder that never came back to patch its
            // header) says "to the end of the file" with it. Judged as
            // declared, every such file read as hours long and was refused
            // however short it was, so it is the file's own length the
            // ceilings below judge for it.
            let size = if size == u32::MAX {
                raf.metadata()?.len().saturating_sub(offset + 8)
            } else {
                u64::from(size)
            };
            // Ceilings judge the DECLARED size first — the whole point of the
            // gate is refusing a big file from its header, before any
            // physical size is consulted.
            let declared_frames = size / u64::from(block_align);
            if declared_frames > cap_frames(MAX_ANALYSIS_DURATION, sample_rate) {
                return Ok(None);
            }
            // Header-independent ceilings (see their doc): bytes bound what
            // the reader streams through, frames bound the f32 analysis
            // buffer.
            if declared_frames > MAX_ANALYSIS_FRAMES {
                return Ok(None);
            }
            if offset + 8 + size > MAX_ANALYSIS_BYTES {
                return Ok(None);
            }
            // Only THEN clamp to what the file physically holds (a
            // hard-killed or lying header must not make the reader wait on
            // missing bytes).
            let file_len = raf.metadata()?.len();
            let data_start = offset + 8;
            if file_len <= data_start {
                return Ok(None);
            }
            let data_len = size.min(file_len - data_start);
            let frames = data_len / u64::from(block_align);
            if frames == 0 {
                return Ok(None);
            }
            return Ok(Some(WavAdmission {
                data_start,
                frames: frames as usize,
                sample_rate: i64::from(sample_rate),
                channels: usize::from(channels),
                bits: usize::from(bits),
                format,
            }));
        }
        let size = u64::from(size);
        offset += 8 + size + (size & 1); // chunks are word-aligned
    }
    Ok(None)
}

/// `v`, or 0 when it is NaN or infinite.
fn finite_or_zero(v: f64) -> f64 {
    if v.is_finite() {
        v
    } else {
        0.0
    }
}

/// Streams the admitted PCM straight into the ONE f32 mono analysis buffer,
/// in bounded frame-aligned chunks (4 MiB). This is the memory-budget half
/// of the gate: the old shape read the whole byte payload first and THEN
/// decoded into f64, so a legal 30-minute stereo float take held ~691 MB of
/// bytes beside ~691 MB of samples — the peak is now the analysis buffer
/// plus one chunk. `Ok(None)` when no frame could be read.
pub fn read_wav_mono_streaming(path: &Path, a: &WavAdmission) -> io::Result<Option<PcmAudio>> {
    let mut out = vec![0.0f32; a.frames];
    let mut frame = 0;
    read_wav_mono_chunked(path, a, |chunk, _| {
        out[frame..frame + chunk.len()].copy_from_slice(chunk);
        frame += chunk.len();
    })?;
    if frame == 0 {
        return Ok(None);
    }
    // A truncated file yields a shorter buffer; keep the honest length.
    out.truncate(frame);
    Ok(Some(PcmAudio::new(out, a.sample_rate)))
}

/// The single decode loop behind every analysis read: streams the admitted
/// PCM as bounded mono f32 chunks (4 MiB of file bytes each) so a consumer
/// that never needs the whole track (the streaming spectrogram) pays only one
/// chunk of memory. [`read_wav_mono_streaming`] is this plus a collecting
/// buffer — one decode path, so the two can never diverge.
///
/// Each chunk comes with its frames' loudest channel (the sample of largest
/// magnitude; the chunk itself for a mono file), for the input-quality scan:
/// averaged into the mono signal, a take recorded on one input of a stereo
/// interface read 6 dB under its peak and never as clipped.
///
/// A file that ends early (truncated on disk) yields what it holds. Fails
/// only when the file cannot be opened or read.
pub fn read_wav_mono_chunked(
    path: &Path,
    a: &WavAdmission,
    mut on_chunk: impl FnMut(&[f32], &[f32]),
) -> io::Result<()> {
    let sample_bytes = a.bits.div_ceil(8);
    let frame_bytes = a.channels * sample_bytes;
    if a.frames == 0 || frame_bytes == 0 {
        // Nothing the gate would admit (Dart's chunk-size clamp threw on a
        // hand-built empty admission): nothing to read.
        return Ok(());
    }
    let mut raf = File::open(path)?;
    raf.seek(SeekFrom::Start(a.data_start))?;
    let frames_per_chunk = (CHUNK_BUDGET / frame_bytes).clamp(1, a.frames);
    let inv = 1.0 / a.channels as f64;
    let mut frame = 0;
    while frame < a.frames {
        let want = (a.frames - frame).min(frames_per_chunk);
        let bytes = read_up_to(&mut raf, want * frame_bytes)?;
        let got = bytes.len() / frame_bytes;
        if got == 0 {
            break; // truncated on disk: keep what exists
        }
        let mut out = vec![0.0f32; got];
        let mut loudest = if a.channels == 1 {
            Vec::new()
        } else {
            vec![0.0f32; got]
        };
        for f in 0..got {
            let mut acc = 0.0f64;
            let mut peak = 0.0f64;
            let base = f * frame_bytes;
            for c in 0..a.channels {
                let o = base + c * sample_bytes;
                let v = match (a.format, a.bits) {
                    (1, 8) => (f64::from(bytes[o]) - 128.0) / 128.0,
                    (1, 16) => f64::from(i16::from_le_bytes([bytes[o], bytes[o + 1]])) / 32768.0,
                    (1, 24) => {
                        let u = u32::from(bytes[o])
                            | (u32::from(bytes[o + 1]) << 8)
                            | (u32::from(bytes[o + 2]) << 16);
                        // Sign-extend the 24-bit value (Dart's toSigned(24)).
                        f64::from(((u << 8) as i32) >> 8) / 8388608.0
                    }
                    (1, 32) => {
                        f64::from(i32::from_le_bytes([
                            bytes[o],
                            bytes[o + 1],
                            bytes[o + 2],
                            bytes[o + 3],
                        ])) / 2147483648.0
                    }
                    // Float samples can carry NaN/Inf (a buggy encoder, a
                    // corrupted file); they must be scrubbed to 0 like the
                    // reference decoder does, or one poisoned sample
                    // propagates through the whole FFT and every derived
                    // metric.
                    (3, 32) => finite_or_zero(f64::from(f32::from_le_bytes([
                        bytes[o],
                        bytes[o + 1],
                        bytes[o + 2],
                        bytes[o + 3],
                    ]))),
                    (3, 64) => finite_or_zero(f64::from_le_bytes([
                        bytes[o],
                        bytes[o + 1],
                        bytes[o + 2],
                        bytes[o + 3],
                        bytes[o + 4],
                        bytes[o + 5],
                        bytes[o + 6],
                        bytes[o + 7],
                    ])),
                    _ => 0.0, // unreachable: the gate admitted the format
                };
                acc += v;
                if v.abs() > peak.abs() {
                    peak = v;
                }
            }
            out[f] = (acc * inv) as f32;
            // Judge finiteness on the NARROWED value: a float64 WAV can carry
            // 1e300 — finite in the double above, infinity once stored into
            // f32 — and one infinity poisons every FFT and metric downstream.
            if !out[f].is_finite() {
                out[f] = 0.0;
            }
            if a.channels != 1 {
                loudest[f] = peak as f32;
                if !loudest[f].is_finite() {
                    loudest[f] = 0.0;
                }
            }
        }
        if a.channels == 1 {
            on_chunk(&out, &out);
        } else {
            on_chunk(&out, &loudest);
        }
        frame += got;
    }
    Ok(())
}

/// Whether a caller that waited its turn at the gate no longer wants the
/// result (its screen closed, or its file changed): the queued analyses of a
/// song left with several lanes open each still ran a full-take compute,
/// holding up the next screen's behind them.
fn unwanted(wanted: Option<&dyn Fn() -> bool>) -> bool {
    wanted.is_some_and(|w| !w())
}

/// Whether [path] is a file (Dart's `File.exists`).
fn is_file(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|m| m.is_file())
}

/// The header-admitted file at [path], or `None` (missing, not a WAV, over
/// the cap).
fn admitted(path: &str) -> Option<(&Path, WavAdmission)> {
    let path = Path::new(path);
    if !is_file(path) {
        return None;
    }
    Some((path, admit_for_analysis(path)?))
}

impl SpectrogramService {
    pub fn new() -> Self {
        SpectrogramService
    }

    /// The spectrogram of the WAV at [path] (bins up to [`AnalysisParams::max_hz`]),
    /// or `None` when the file is missing, not an admitted WAV, holds no
    /// frame, or is no longer [wanted] once the gate admits the call. `Err`
    /// when the file cannot be read after its header was admitted.
    ///
    /// FULLY windowed: decode chunks feed the incremental STFT directly, so
    /// the whole-track sample buffer never exists (a 10-minute take's
    /// ~115 MB master buffer becomes one 4 MiB chunk beside the columns).
    /// Gated: heavy analyses queue behind the app-wide gate instead of
    /// stacking peaks.
    pub fn compute_from_file(
        &self,
        path: &str,
        params: AnalysisParams,
        wanted: Option<&dyn Fn() -> bool>,
    ) -> io::Result<Option<Arc<Spectrogram>>> {
        let Some((path, admission)) = admitted(path) else {
            return Ok(None);
        };
        analysis_gate().run(|| {
            if unwanted(wanted) {
                return Ok(None);
            }
            Ok(spectrogram_of(path, &admission, params)?.map(Arc::new))
        })
    }

    /// Per-frame voice-timbre metrics (ring/tilt/CPP/formants) for a take,
    /// with [`Self::compute_from_file`]'s support and gating. The metrics
    /// ignore [`AnalysisParams::max_hz`] (Dart's signature had none).
    pub fn compute_metrics_from_file(
        &self,
        path: &str,
        params: AnalysisParams,
        wanted: Option<&dyn Fn() -> bool>,
    ) -> io::Result<Option<Arc<VoiceMetrics>>> {
        let Some((path, admission)) = admitted(path) else {
            return Ok(None);
        };
        analysis_gate().run(|| {
            if unwanted(wanted) {
                return Ok(None);
            }
            // Windowed like compute_both_from_file, minus the spectrogram.
            Ok(take_analysis_of(path, &admission, params, false)?.map(|a| Arc::new(a.metrics)))
        })
    }

    /// The spectrogram AND the voice metrics from a single decode, in a
    /// single compute. The two analyses always run together when a take is
    /// selected; computing them separately used to decode the same file
    /// twice at the same moment — twice the bytes, twice the PCM. `None`
    /// when the file is missing or over the analysis cap; each field is
    /// `None` when its compute failed. The pair's LTAS and voice profile ride
    /// along (see [`TakeAnalysisBundle`]).
    pub fn compute_both_from_file(
        &self,
        path: &str,
        params: AnalysisParams,
        wanted: Option<&dyn Fn() -> bool>,
    ) -> io::Result<Option<TakeAnalysisBundle>> {
        let Some((path, admission)) = admitted(path) else {
            return Ok(None);
        };
        analysis_gate().run(|| {
            if unwanted(wanted) {
                return Ok(None);
            }
            let Some(StreamedTakeAnalysis {
                spectrogram,
                metrics,
            }) = take_analysis_of(path, &admission, params, true)?
            else {
                return Ok(None);
            };
            let ltas = spectrogram
                .as_ref()
                .map(|s| Arc::new(compute_ltas(s, Some(&metrics))));
            let profile = analyze_voice_profile(&metrics, VoiceProfileOptions::default());
            Ok(Some(TakeAnalysisBundle {
                spectrogram: spectrogram.map(Arc::new),
                metrics: Some(Arc::new(metrics)),
                ltas,
                profile: Some(Arc::new(profile)),
            }))
        })
    }
}

/// [`SpectrogramService::compute_from_file`]'s compute.
fn spectrogram_of(
    path: &Path,
    admission: &WavAdmission,
    params: AnalysisParams,
) -> io::Result<Option<Spectrogram>> {
    let mut builder = StreamingSpectrogramBuilder::new(
        admission.sample_rate,
        params,
        // The header gave the frame count: the image is sized once instead
        // of held twice (see StreamingSpectrogramBuilder).
        stft_frame_count(admission.frames, params.fft_size, params.hop),
    );
    let mut any = false;
    read_wav_mono_chunked(path, admission, |chunk, _| {
        any = true;
        builder.add(chunk);
    })?;
    Ok(any.then(|| builder.finish()))
}

/// The metrics compute, with the spectrogram when [spectrogram]: FULLY
/// windowed, decode chunks feed the streaming analyzer (whose per-frame math
/// IS the batch `compute_voice_metrics`, pinned bit-identical by test) —
/// neither the whole-track sample buffer nor the full decimated copies ever
/// exist. The resident set is a few analysis windows plus the RESULTS.
fn take_analysis_of(
    path: &Path,
    admission: &WavAdmission,
    params: AnalysisParams,
    spectrogram: bool,
) -> io::Result<Option<StreamedTakeAnalysis>> {
    let expected_frames = stft_frame_count(admission.frames, params.fft_size, params.hop);
    let mut analyzer =
        StreamingTakeAnalyzer::new(admission.sample_rate, params, expected_frames, spectrogram);
    let mut any = false;
    read_wav_mono_chunked(path, admission, |chunk, loudest| {
        any = true;
        analyzer.add(chunk, Some(loudest));
    })?;
    Ok(any.then(|| analyzer.finish()))
}

impl SpectrogramAnalyses for SpectrogramService {
    fn compute_from_file(
        &self,
        path: &str,
        params: AnalysisParams,
        wanted: Option<&dyn Fn() -> bool>,
    ) -> io::Result<Option<Arc<Spectrogram>>> {
        SpectrogramService::compute_from_file(self, path, params, wanted)
    }

    fn compute_metrics_from_file(
        &self,
        path: &str,
        params: AnalysisParams,
        wanted: Option<&dyn Fn() -> bool>,
    ) -> io::Result<Option<Arc<VoiceMetrics>>> {
        SpectrogramService::compute_metrics_from_file(self, path, params, wanted)
    }

    fn compute_both_from_file(
        &self,
        path: &str,
        params: AnalysisParams,
        wanted: Option<&dyn Fn() -> bool>,
    ) -> io::Result<Option<TakeAnalysisBundle>> {
        SpectrogramService::compute_both_from_file(self, path, params, wanted)
    }
}

#[cfg(test)]
mod tests {
    //! Ports of spectrogram_gate_test.dart (the admission gate and the
    //! streaming reader; its reference-decoder halves are audio_dsp's),
    //! spectrogram_wanted_test.dart, the admission-gate half of
    //! wav_malformed_input_test.dart (the probe and the head trim are
    //! audio_file_service's and audio_edit_service's), and, as ignored manual
    //! runs, spectrogram_memory_test.dart and measure_analysis_peak_test.dart.

    use std::cell::Cell;
    use std::f64::consts::PI;
    use std::path::PathBuf;
    use std::rc::Rc;

    use tempfile::TempDir;
    use vocara_model::models::voice_metrics::analyze_voice_profile;

    use super::*;
    use crate::audio_dsp::{
        compute_spectrogram, compute_voice_metrics, read_wav_mono, write_wav_mono,
    };

    /// A RIFF/WAVE byte builder for hand-made headers.
    #[derive(Default)]
    struct Riff(Vec<u8>);

    impl Riff {
        fn str(&mut self, s: &str) -> &mut Self {
            self.0.extend_from_slice(s.as_bytes());
            self
        }
        fn u32(&mut self, v: u32) -> &mut Self {
            self.0.extend_from_slice(&v.to_le_bytes());
            self
        }
        fn u16(&mut self, v: u16) -> &mut Self {
            self.0.extend_from_slice(&v.to_le_bytes());
            self
        }
        fn bytes(&mut self, b: &[u8]) -> &mut Self {
            self.0.extend_from_slice(b);
            self
        }
    }

    /// Builds a RIFF/WAVE header with an arbitrary chunk sequence and a
    /// `data` chunk whose DECLARED size is [declared_data_bytes] — the
    /// payload itself is not written (the gate must judge the header, never
    /// load the body).
    fn wav_header(
        sample_rate: u32,
        block_align: u16,
        declared_data_bytes: u32,
        extensible: bool,
        extra_list_chunk: bool,
    ) -> Vec<u8> {
        let mut b = Riff::default();
        b.str("RIFF").u32(0xFFFF_FFF0); // declared RIFF size is irrelevant to the gate
        b.str("WAVE");
        if extra_list_chunk {
            b.str("LIST").u32(12).bytes(&[0; 12]); // metadata the walk must seek over
        }
        b.str("fmt ").u32(if extensible { 40 } else { 16 });
        b.u16(if extensible { 0xFFFE } else { 1 });
        b.u16(1); // channels
        b.u32(sample_rate)
            .u32(sample_rate * u32::from(block_align))
            .u16(block_align)
            .u16(16);
        if extensible {
            b.u16(22); // cbSize
            b.u16(16); // valid bits
            b.u32(0x4); // channel mask
                        // PCM SubFormat GUID
            b.bytes(&[
                0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, //
                0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71, //
            ]);
        }
        b.str("data").u32(declared_data_bytes);
        b.0
    }

    /// A canonical header with 8 channels of 32-bit samples (blockAlign 32,
    /// consistent) — for exercising the absolute byte ceiling.
    fn wav_header_8ch(sample_rate: u32, declared_data_bytes: u32) -> Vec<u8> {
        let mut b = Riff::default();
        b.str("RIFF").u32(0xFFFF_FFF0).str("WAVE");
        b.str("fmt ").u32(16);
        b.u16(1); // PCM
        b.u16(8); // channels
        b.u32(sample_rate).u32(sample_rate * 32);
        b.u16(32); // blockAlign = 8 * 4
        b.u16(32); // bits
        b.str("data").u32(declared_data_bytes);
        b.0
    }

    /// A canonical header of [format]/[channels]/[bits] at [rate] over
    /// [data].
    fn wav(format: u16, channels: u16, rate: u32, bits: u16, data: &[u8]) -> Vec<u8> {
        let block_align = channels * bits.div_ceil(8);
        let mut b = Riff::default();
        b.str("RIFF").u32(36 + data.len() as u32).str("WAVE");
        b.str("fmt ").u32(16).u16(format).u16(channels);
        b.u32(rate)
            .u32(rate * u32::from(block_align))
            .u16(block_align)
            .u16(bits);
        b.str("data").u32(data.len() as u32).bytes(data);
        b.0
    }

    /// A 1 s, 48 kHz, 16-bit stereo WAV with the singer on the LEFT input
    /// only (what a 2-input interface records as a stereo file), driven into
    /// clipping; the right channel is silent.
    fn one_sided_clipped_stereo(dir: &Path) -> PathBuf {
        let sr = 48000u32;
        let mut data = Vec::with_capacity(sr as usize * 4);
        for i in 0..sr {
            let v =
                (1.6 * (2.0 * PI * 220.0 * f64::from(i) / f64::from(sr)).sin()).clamp(-1.0, 1.0);
            data.extend_from_slice(&((v * 32767.0).round() as i16).to_le_bytes());
            data.extend_from_slice(&0i16.to_le_bytes());
        }
        let path = dir.join("one_sided.wav");
        fs::write(&path, wav(1, 2, sr, 16, &data)).unwrap();
        path
    }

    /// A temp dir holding `probe.wav` of [bytes].
    fn write(bytes: &[u8]) -> (TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("probe.wav");
        fs::write(&path, bytes).unwrap();
        (dir, path)
    }

    fn zeros(n: usize) -> Vec<u8> {
        vec![0; n]
    }

    fn cat(a: &[u8], b: &[u8]) -> Vec<u8> {
        [a, b].concat()
    }

    // 10 min cap @ 44.1 kHz mono 16-bit = 44100 * 2 bytes * 600 s.
    const UNDER_CAP: u32 = 44100 * 2 * 60; // 1 minute
    const OVER_CAP: u32 = 44100 * 2 * 660; // 11 minutes

    #[test]
    fn a_canonical_wav_under_the_cap_is_admitted() {
        // The gate clamps the declared payload to what the file physically
        // holds, so the admitted fixture needs its bytes actually present.
        let header = wav_header(44100, 2, UNDER_CAP, false, false);
        let (_dir, f) = write(&cat(&header, &zeros(UNDER_CAP as usize)));
        assert_eq!(
            admit_for_analysis(&f),
            Some(WavAdmission {
                data_start: 44,
                frames: UNDER_CAP as usize / 2,
                sample_rate: 44100,
                channels: 1,
                bits: 16,
                format: 1,
            })
        );
    }

    #[test]
    fn an_extensible_wav_with_extra_chunks_is_still_measured_and_skipped_over_the_cap() {
        // The old canonical-44-byte probe returned "unknown" for exactly this
        // shape and the whole file was then loaded; the chunk walk must both
        // admit a short one and refuse a long one.
        let header = wav_header(44100, 2, UNDER_CAP, true, true);
        let (_short_dir, short) = write(&cat(&header, &zeros(UNDER_CAP as usize)));
        let admitted = admit_for_analysis(&short).expect("a short extensible WAV is admitted");
        assert_eq!(admitted.format, 1, "resolved through the SubFormat");
        assert_eq!(admitted.data_start, header.len() as u64);

        let (_long_dir, long) = write(&wav_header(44100, 2, OVER_CAP, true, true));
        assert_eq!(admit_for_analysis(&long), None);
    }

    #[test]
    fn a_canonical_wav_over_the_cap_is_skipped_from_the_header_alone() {
        // The file holds only the header: judging by size-on-disk would admit
        // it; the gate must trust the declared data length instead.
        let (_dir, f) = write(&wav_header(44100, 2, OVER_CAP, false, false));
        assert_eq!(admit_for_analysis(&f), None);
    }

    /// A streamed WAV's data size is 0xFFFFFFFF, "to the end of the file":
    /// a short one is admitted for what it holds, a long one still refused.
    #[test]
    fn a_streamed_wav_without_a_data_size_is_judged_by_its_length() {
        let header = wav_header(44100, 2, u32::MAX, false, false);
        let (_dir, f) = write(&cat(&header, &zeros(UNDER_CAP as usize)));
        assert_eq!(
            admit_for_analysis(&f).map(|a| a.frames),
            Some(UNDER_CAP as usize / 2)
        );

        let (_long_dir, long) = write(&header);
        let file = fs::OpenOptions::new().write(true).open(&long).unwrap();
        file.set_len(header.len() as u64 + u64::from(OVER_CAP))
            .unwrap();
        drop(file);
        assert_eq!(admit_for_analysis(&long), None);
    }

    #[test]
    fn non_wav_files_are_skipped_without_reading_the_body() {
        let (_dir, f) = write(&[7; 4096]);
        assert_eq!(admit_for_analysis(&f), None);
    }

    #[test]
    fn a_missing_file_is_skipped_not_crashed_on() {
        assert_eq!(admit_for_analysis(Path::new("/nonexistent/x.wav")), None);
    }

    #[test]
    fn a_forged_block_align_cannot_talk_the_frame_count_under_the_cap() {
        // channels=1, bits=16 → a truthful blockAlign is 2. A forged huge
        // value divides the declared size down to a tiny frame count that
        // sails under the duration cap — while the admitted READ would still
        // be the full multi-gigabyte data chunk. The consistency check must
        // refuse it.
        let (_dir, f) = write(&wav_header(44100, 4096, OVER_CAP, false, false));
        assert_eq!(admit_for_analysis(&f), None);
    }

    #[test]
    fn headers_outside_the_supported_envelope_are_refused_absurd_rate() {
        // No real recording; inflates every rate-derived cap.
        let (_dir, f) = write(&wav_header(4_000_000, 2, UNDER_CAP, false, false));
        assert_eq!(admit_for_analysis(&f), None);
    }

    #[test]
    fn a_rate_above_the_voice_analysis_envelope_is_refused_192_khz() {
        // A YIN analysis frame spans fft_size / sample_rate seconds, but the
        // fmin search lag is a fixed 1/63 s — above ~129 kHz the lag no longer
        // fits the frame and EVERY frame bails out unvoiced regardless of
        // content (see MAX_ANALYSIS_SAMPLE_RATE). Admitting such a file used to
        // render a healthy spectrogram beside silently empty voice analyses;
        // the gate must refuse it like any other un-analyzable header. Body
        // present so the refusal is for the rate, not for missing bytes.
        let declared = 192000 * 2 * 30; // 30 s mono PCM16 @ 192 kHz
        let header = wav_header(192000, 2, declared, false, false);
        let (_dir, f) = write(&cat(&header, &zeros(declared as usize)));
        assert_eq!(admit_for_analysis(&f), None);
        // ...while the envelope's top rate is admitted.
        let declared = 96000 * 2;
        let header = wav_header(96000, 2, declared, false, false);
        let (_dir, f) = write(&cat(&header, &zeros(declared as usize)));
        assert!(admit_for_analysis(&f).is_some());
    }

    #[test]
    fn the_header_independent_byte_ceiling_holds_even_when_the_per_rate_duration_math_would_admit()
    {
        // 8 channels of 32-bit at 44.1 kHz: blockAlign 32 is CONSISTENT, the
        // declared duration (4 min) is comfortably under the 10-minute cap
        // AND the frame count is under the frame ceiling — but the byte total
        // (~339 MB) crosses MAX_ANALYSIS_BYTES. Only the absolute byte
        // ceiling can refuse this header.
        let declared: u32 = 4 * 60 * 44100 * 32; // 338,688,000 bytes
        assert!(u64::from(declared) > MAX_ANALYSIS_BYTES);
        let header = wav_header_8ch(44100, declared);
        // The payload must PHYSICALLY exist: a header-only file is refused by
        // the physical-availability clamp regardless, which would leave the
        // byte ceiling untested. A sparse file gives the declared length
        // without 339 MB of disk I/O.
        let (_dir, f) = write(&header);
        let file = fs::OpenOptions::new().write(true).open(&f).unwrap();
        file.set_len(header.len() as u64 + u64::from(declared))
            .unwrap();
        drop(file);
        assert_eq!(
            fs::metadata(&f).unwrap().len(),
            header.len() as u64 + u64::from(declared),
            "the fixture must be physically as long as it declares"
        );
        assert_eq!(admit_for_analysis(&f), None);
    }

    #[test]
    fn the_admitted_length_stops_at_the_end_of_the_data_chunk_trailing_junk_is_never_read() {
        // A small honest data chunk followed by a pile of appended garbage:
        // the gate must admit the file but cap the read at the data chunk's
        // end, so the junk cannot ride into memory on the strength of the
        // honest header.
        let header = wav_header(44100, 2, UNDER_CAP, false, false);
        let bytes = [
            header.as_slice(),
            &zeros(UNDER_CAP as usize), // the actual data payload
            &[0x5A; 1 << 20],           // trailing junk
        ]
        .concat();
        let (_dir, f) = write(&bytes);
        let admission = admit_for_analysis(&f).expect("admitted");
        assert_eq!(
            admission.data_start + admission.frames as u64 * 2,
            header.len() as u64 + u64::from(UNDER_CAP),
            "the admitted PCM ends exactly at the data chunk boundary"
        );
        assert!(
            admission.data_start + (admission.frames as u64) * 2 < fs::metadata(&f).unwrap().len(),
            "the junk tail must sit past the admitted region"
        );
    }

    #[test]
    fn a_header_that_promises_more_than_the_file_holds_is_read_for_what_it_holds() {
        // A hard-killed recording: the header claims a full minute, the disk
        // holds 1000 frames and half of another.
        let header = wav_header(44100, 2, UNDER_CAP, false, false);
        let (_dir, f) = write(&cat(&header, &zeros(2001)));
        let a = admit_for_analysis(&f).expect("admitted for what it holds");
        assert_eq!(a.frames, 1000);
        let pcm = read_wav_mono_streaming(&f, &a).unwrap().unwrap();
        assert_eq!(pcm.samples.len(), 1000);
    }

    // --- readWavMonoStreaming equals the reference decoder ------------------

    fn check(name: &str, bytes: &[u8]) {
        let (_dir, f) = write(bytes);
        let admission = admit_for_analysis(&f).unwrap_or_else(|| panic!("{name} must be admitted"));
        let streamed = read_wav_mono_streaming(&f, &admission)
            .unwrap()
            .unwrap_or_else(|| panic!("{name}: streamed"));
        let reference = read_wav_mono(bytes).unwrap_or_else(|| panic!("{name}: reference"));
        assert_eq!(streamed.sample_rate, reference.sample_rate, "{name}");
        assert_eq!(streamed.samples.len(), reference.samples.len(), "{name}");
        for (i, (s, r)) in streamed.samples.iter().zip(&reference.samples).enumerate() {
            assert!(
                (f64::from(*s) - f64::from(*r)).abs() <= 1e-9,
                "{name} sample {i}: {s} vs {r}"
            );
        }
    }

    #[test]
    fn streaming_equals_the_reference_mono_pcm16() {
        let samples: Vec<f64> = (0..500)
            .map(|i| (f64::from(i) / 25.0).sin() * 0.8)
            .collect();
        check("mono pcm16", &write_wav_mono(&samples, 8000));
    }

    #[test]
    fn streaming_equals_the_reference_extensible_pcm16_with_extra_chunks_stereo_downmix() {
        // Interleaved stereo with distinct channels, so the downmix averaging
        // is genuinely exercised.
        let mut data = Vec::with_capacity(400 * 4);
        for i in 0..400 {
            let t = f64::from(i) / 10.0;
            data.extend_from_slice(&((t.sin() * 12000.0).round() as i16).to_le_bytes());
            data.extend_from_slice(&((t.cos() * 9000.0).round() as i16).to_le_bytes());
        }
        let mut b = Riff::default();
        b.str("RIFF")
            .u32(4 + 12 + (8 + 40) + (8 + data.len() as u32))
            .str("WAVE");
        b.str("LIST").u32(4).bytes(&[0; 4]);
        b.str("fmt ").u32(40).u16(0xFFFE);
        b.u16(2); // channels
        b.u32(8000).u32(8000 * 4);
        b.u16(4); // blockAlign
        b.u16(16).u16(22).u16(16).u32(3);
        b.u16(1); // SubFormat = PCM
        b.bytes(&[
            0x00, 0x00, 0x00, 0x00, 0x10, 0x00, //
            0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71, //
        ]);
        b.str("data").u32(data.len() as u32).bytes(&data);
        check("extensible stereo pcm16", &b.0);
    }

    #[test]
    fn integer_pcm64_is_refused_at_the_gate() {
        // No decoder branch exists: admitting it decoded a whole track of
        // silence.
        let mut b = Riff::default();
        b.str("RIFF").u32(36 + 80).str("WAVE");
        b.str("fmt ").u32(16);
        b.u16(1); // integer PCM…
        b.u16(1).u32(48000).u32(48000 * 8);
        b.u16(8); // blockAlign = 1 ch * 8 bytes
        b.u16(64); // …at 64 bits: not a real format
        b.str("data").u32(80).bytes(&[0; 80]);
        let (_dir, f) = write(&b.0);
        assert_eq!(admit_for_analysis(&f), None);
    }

    #[test]
    fn non_finite_float_samples_are_scrubbed_to_zero_matching_the_reference_decoder() {
        let mut data = Vec::new();
        for v in [
            0.5f32,
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            -0.25,
            0.125,
        ] {
            data.extend_from_slice(&v.to_le_bytes());
        }
        let bytes = wav(3, 1, 48000, 32, &data);
        check("float32 with NaN/Inf poison", &bytes);
        // Belt-and-braces beyond the equivalence: the poisoned slots really
        // are zero, not merely "whatever the reference produced".
        let (_dir, f) = write(&bytes);
        let admission = admit_for_analysis(&f).unwrap();
        let pcm = read_wav_mono_streaming(&f, &admission).unwrap().unwrap();
        assert_eq!(pcm.samples[1], 0.0);
        assert_eq!(pcm.samples[2], 0.0);
        assert_eq!(pcm.samples[3], 0.0);
        assert!((f64::from(pcm.samples[0]) - 0.5).abs() <= 1e-9);
    }

    #[test]
    fn streaming_equals_the_reference_float32_the_take_format() {
        let mut data = Vec::new();
        for i in 0..300 {
            // Includes an over-unity peak, legal in float takes.
            data.extend_from_slice(&(((f64::from(i) / 7.0).sin() * 1.2) as f32).to_le_bytes());
        }
        check("mono float32", &wav(3, 1, 48000, 32, &data));
    }

    #[test]
    fn float64_finite_but_huge_samples_cannot_smuggle_infinity_through_the_f32_narrowing() {
        // 1e300 is finite in the double the f64 branch reads — but infinity
        // once stored into the f32 analysis buffer. The finiteness check must
        // judge the NARROWED value, or one such sample poisons every FFT and
        // metric downstream.
        let mut data = Vec::new();
        for v in [0.5f64, 1e300, -1e300, f64::NAN, 0.25, -0.125] {
            data.extend_from_slice(&v.to_le_bytes());
        }
        let (_dir, f) = write(&wav(3, 1, 48000, 64, &data));
        let admission = admit_for_analysis(&f).unwrap();
        let pcm = read_wav_mono_streaming(&f, &admission).unwrap().unwrap();
        assert!(
            pcm.samples.iter().all(|s| s.is_finite()),
            "no sample may narrow into infinity"
        );
        assert_eq!(pcm.samples[1], 0.0);
        assert_eq!(pcm.samples[2], 0.0);
        assert_eq!(pcm.samples[3], 0.0);
        assert!((f64::from(pcm.samples[0]) - 0.5).abs() <= 1e-9);
        assert!((f64::from(pcm.samples[4]) - 0.25).abs() <= 1e-9);
    }

    #[test]
    fn every_integer_depth_decodes_to_the_same_scale_and_reports_its_loudest_channel() {
        // The formats the reference decoder does not read (8/24/32-bit
        // integer), against their definitions: unsigned 8-bit around 128,
        // signed little-endian otherwise, over 2^(bits-1); the loudest
        // channel is the sample of largest magnitude, sign kept.
        let cases: [(u16, Vec<u8>, [f64; 2]); 3] = [
            (8, vec![255, 0, 64, 192], [127.0 / 128.0, -1.0]),
            (
                24,
                vec![
                    0xFF, 0xFF, 0x7F, 0x00, 0x00, 0x80, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x40,
                ],
                [8388607.0 / 8388608.0, -1.0],
            ),
            (
                32,
                [
                    i32::MAX.to_le_bytes(),
                    i32::MIN.to_le_bytes(),
                    (-(1i32 << 30)).to_le_bytes(),
                    (1i32 << 30).to_le_bytes(),
                ]
                .concat(),
                [2147483647.0 / 2147483648.0, -1.0],
            ),
        ];
        for (bits, data, [l0, r0]) in cases {
            let (_dir, f) = write(&wav(1, 2, 8000, bits, &data));
            let a = admit_for_analysis(&f).unwrap_or_else(|| panic!("{bits}-bit admitted"));
            assert_eq!((a.channels, a.bits, a.frames), (2, usize::from(bits), 2));
            let mut chunks = Vec::new();
            read_wav_mono_chunked(&f, &a, |chunk, loudest| {
                chunks.push((chunk.to_vec(), loudest.to_vec()));
            })
            .unwrap();
            assert_eq!(chunks.len(), 1, "{bits}-bit");
            let (mono, loudest) = &chunks[0];
            assert_eq!(mono[0], ((l0 + r0) * 0.5) as f32, "{bits}-bit frame 0");
            assert_eq!(mono[1], ((-0.5 + 0.5) * 0.5) as f32, "{bits}-bit frame 1");
            assert_eq!(
                loudest[0], r0 as f32,
                "{bits}-bit: the louder channel, sign kept"
            );
            assert_eq!(
                loudest[1], -0.5,
                "{bits}-bit: the first of equal magnitudes"
            );
        }
    }

    #[test]
    fn a_mono_chunk_is_its_own_loudest_channel() {
        let (_dir, f) = write(&write_wav_mono(&[0.5f64, -0.25], 8000));
        let a = admit_for_analysis(&f).unwrap();
        read_wav_mono_chunked(&f, &a, |chunk, loudest| {
            assert!(std::ptr::eq(chunk, loudest));
        })
        .unwrap();
    }

    // --- the computes -------------------------------------------------------

    /// spectrogram_gate_test's take: a sung-ish tone with a breath gap, so
    /// the LTAS voicing gate matters.
    fn sung_take(dir: &Path) -> PathBuf {
        let sr = 48000;
        let samples: Vec<f64> = (0..sr * 2)
            .map(|i| {
                let t = f64::from(i) / f64::from(sr);
                if t % 1.0 < 0.8 {
                    0.4 * (2.0 * PI * (220.0 + 6.0 * (2.0 * PI * 5.0 * t).sin()) * t).sin()
                } else {
                    0.0
                }
            })
            .collect();
        let path = dir.join("take.wav");
        fs::write(&path, write_wav_mono(&samples, i64::from(sr))).unwrap();
        path
    }

    #[test]
    fn compute_both_brings_the_ltas_and_voice_profile_of_exactly_the_pair_it_returns() {
        let dir = tempfile::tempdir().unwrap();
        let path = sung_take(dir.path());
        let both = SpectrogramService
            .compute_both_from_file(&path.to_string_lossy(), AnalysisParams::default(), None)
            .unwrap()
            .unwrap();
        let spec = both.spectrogram.as_ref().unwrap();
        let metrics = both.metrics.as_ref().unwrap();
        let ltas = compute_ltas(spec, Some(metrics));
        let profile = analyze_voice_profile(metrics, VoiceProfileOptions::default());

        let got = both.ltas.as_ref().unwrap();
        assert_eq!(got.db, ltas.db);
        assert_eq!(got.frame_count, ltas.frame_count);
        assert_eq!(got.alpha_ratio, ltas.alpha_ratio);
        assert_eq!(got.singers_formant_db, ltas.singers_formant_db);
        assert!(got.frame_count < spec.frames); // voiced-gated
        let p = both.profile.as_ref().unwrap();
        assert_eq!(
            [p.low_midi, p.high_midi, p.tess_lo_midi, p.tess_hi_midi],
            [
                profile.low_midi,
                profile.high_midi,
                profile.tess_lo_midi,
                profile.tess_hi_midi
            ]
        );
        assert_eq!(p.passaggio_midi, profile.passaggio_midi);
        assert_eq!(
            [p.soft_db, p.loud_db, p.median_db],
            [profile.soft_db, profile.loud_db, profile.median_db]
        );
    }

    #[test]
    fn the_computes_are_the_batch_analyses_of_the_streamed_samples() {
        // The windowed computes are pinned bit-identical to the batch
        // functions in audio_dsp; this pins the wiring (the admission's rate
        // and frame count, the params) end to end.
        let dir = tempfile::tempdir().unwrap();
        let path = sung_take(dir.path());
        let a = admit_for_analysis(&path).unwrap();
        let pcm = read_wav_mono_streaming(&path, &a).unwrap().unwrap();
        let p = path.to_string_lossy();
        for params in [
            AnalysisParams::default(),
            AnalysisParams {
                fft_size: 1024,
                hop: 256,
                max_hz: 18000.0,
            },
        ] {
            let want_spec = compute_spectrogram(&pcm.samples, pcm.sample_rate, params);
            let want_metrics = compute_voice_metrics(&pcm.samples, pcm.sample_rate, params);
            let spec = SpectrogramService
                .compute_from_file(&p, params, None)
                .unwrap()
                .unwrap();
            assert_eq!(spec.db, want_spec.db);
            assert_eq!(
                (spec.frames, spec.bins, spec.max_hz),
                (want_spec.frames, want_spec.bins, want_spec.max_hz)
            );
            let metrics = SpectrogramService
                .compute_metrics_from_file(&p, params, None)
                .unwrap()
                .unwrap();
            let both = SpectrogramService
                .compute_both_from_file(&p, params, None)
                .unwrap()
                .unwrap();
            assert_eq!(both.spectrogram.as_ref().unwrap().db, want_spec.db);
            for m in [&metrics, both.metrics.as_ref().unwrap()] {
                assert_eq!(m.frames, want_metrics.frames);
                assert_eq!(m.frames, spec.frames);
                for (curve, want) in [
                    (&m.f0, &want_metrics.f0),
                    (&m.ring, &want_metrics.ring),
                    (&m.loudness, &want_metrics.loudness),
                ] {
                    let bits = |c: &Vec<f32>| c.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
                    assert_eq!(bits(curve), bits(want));
                }
                assert_eq!(m.active, want_metrics.active);
            }
        }
    }

    // Averaged into the mono analysis signal, a full-scale channel beside a
    // silent one read 6 dB under its peak, and the "Clipped" warning never
    // showed for a take recorded on one input of a stereo interface.
    #[test]
    fn a_take_clipped_on_one_channel_of_a_stereo_file_reads_as_clipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = one_sided_clipped_stereo(dir.path());
        let p = path.to_string_lossy();
        let m = SpectrogramService
            .compute_metrics_from_file(&p, AnalysisParams::default(), None)
            .unwrap()
            .unwrap();
        assert!(
            m.peak_dbfs > -0.5,
            "the left channel peaks: {}",
            m.peak_dbfs
        );
        assert!(m.is_clipped(), "half its samples are flat-topped");
        let both = SpectrogramService
            .compute_both_from_file(&p, AnalysisParams::default(), None)
            .unwrap()
            .unwrap();
        assert!(both.metrics.unwrap().is_clipped());
    }

    #[test]
    fn a_missing_or_unadmitted_file_computes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let not_wav = dir.path().join("clip.m4a");
        fs::write(&not_wav, [7u8; 4096]).unwrap();
        for path in [
            dir.path().join("gone.wav"),
            not_wav,
            dir.path().to_path_buf(), // a directory is no file
        ] {
            let p = path.to_string_lossy();
            let s = SpectrogramService;
            let params = AnalysisParams::default();
            assert!(s.compute_from_file(&p, params, None).unwrap().is_none());
            assert!(s
                .compute_metrics_from_file(&p, params, None)
                .unwrap()
                .is_none());
            assert!(s
                .compute_both_from_file(&p, params, None)
                .unwrap()
                .is_none());
        }
    }

    // --- spectrogram_wanted_test ---------------------------------------------
    //
    // An analysis queued behind the gate can stop being wanted before its turn
    // (its screen left, its file changed): it is skipped instead of run for no
    // one. The predicate closes over a view model, which must not be moved
    // into the compute.

    /// What a `wanted` predicate closes over: not `Send`, like a view model.
    struct Owner {
        still_wanted: Rc<Cell<bool>>,
    }

    fn wanted_fixture() -> (TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("take.wav");
        let rate = 16000;
        let samples: Vec<f64> = (0..rate)
            .map(|i| 0.3 * (2.0 * PI * 220.0 * f64::from(i) / f64::from(rate)).sin())
            .collect();
        fs::write(&path, write_wav_mono(&samples, i64::from(rate))).unwrap();
        let path = path.to_string_lossy().into_owned();
        (dir, path)
    }

    #[test]
    fn a_wanted_analysis_runs_its_predicate_kept_on_the_calling_thread() {
        let (_dir, path) = wanted_fixture();
        let owner = Owner {
            still_wanted: Rc::new(Cell::new(true)),
        };
        let wanted = || owner.still_wanted.get();
        let (s, params) = (SpectrogramService, AnalysisParams::default());
        assert!(s
            .compute_from_file(&path, params, Some(&wanted))
            .unwrap()
            .is_some());
        assert!(s
            .compute_metrics_from_file(&path, params, Some(&wanted))
            .unwrap()
            .is_some());
        assert!(s
            .compute_both_from_file(&path, params, Some(&wanted))
            .unwrap()
            .is_some());
    }

    #[test]
    fn an_unwanted_one_is_skipped() {
        let (_dir, path) = wanted_fixture();
        let owner = Owner {
            still_wanted: Rc::new(Cell::new(false)),
        };
        let wanted = || owner.still_wanted.get();
        let (s, params) = (SpectrogramService, AnalysisParams::default());
        assert!(s
            .compute_from_file(&path, params, Some(&wanted))
            .unwrap()
            .is_none());
        assert!(s
            .compute_metrics_from_file(&path, params, Some(&wanted))
            .unwrap()
            .is_none());
        assert!(s
            .compute_both_from_file(&path, params, Some(&wanted))
            .unwrap()
            .is_none());
    }

    #[test]
    fn the_service_is_shared_with_worker_threads() {
        fn send_sync<T: Send + Sync + ?Sized>() {}
        send_sync::<SpectrogramService>();
        send_sync::<dyn SpectrogramAnalyses>();
        let (_dir, path) = wanted_fixture();
        let service: Arc<dyn SpectrogramAnalyses> = Arc::new(SpectrogramService::new());
        let worker = std::thread::spawn(move || {
            service.compute_both_from_file(&path, AnalysisParams::default(), None)
        });
        let both = worker.join().unwrap().unwrap().unwrap();
        assert!(both.spectrogram.is_some() && both.ltas.is_some() && both.profile.is_some());
    }

    // --- wav_malformed_input_test: the analysis gate --------------------------
    //
    // Every WAV byte the app parses came from somewhere it does not control.
    // The contract: a malformed file is REJECTED, never a crash, a hang, or an
    // out-of-range read; a header that lies about its sizes must never be
    // trusted past the real end of the file.

    /// A canonical mono float32 WAV, the exact shape the recorder writes.
    fn good_wav() -> Vec<u8> {
        wav(3, 1, 48000, 32, &zeros(240 * 4))
    }

    /// The gate on [bytes], and the read of whatever it admits: never a
    /// panic, and an admitted region always inside the file.
    fn parse(bytes: &[u8]) -> Option<WavAdmission> {
        let (_dir, f) = write(bytes);
        let a = admit_for_analysis(&f)?;
        let block_align = (a.channels * a.bits.div_ceil(8)) as u64;
        assert!(a.data_start + a.frames as u64 * block_align <= bytes.len() as u64);
        let pcm = read_wav_mono_streaming(&f, &a).unwrap();
        assert!(pcm.is_some_and(|p| p.samples.len() == a.frames));
        Some(a)
    }

    fn put16(b: &mut [u8], off: usize, v: u16) {
        b[off..off + 2].copy_from_slice(&v.to_le_bytes());
    }

    fn put32(b: &mut [u8], off: usize, v: u32) {
        b[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }

    #[test]
    fn a_canonical_file_is_accepted() {
        assert!(parse(&good_wav()).is_some());
    }

    #[test]
    fn truncation_at_every_prefix_length_is_rejected_never_a_crash() {
        // Every boundary the chunk walk can land on: inside RIFF, inside fmt,
        // at the data header, and part way through the payload.
        let good = good_wav();
        for n in [0, 1, 4, 11, 12, 15, 20, 36, 40, 43, 44, 45, 100] {
            let admitted = parse(&good[..n]);
            if n <= 47 {
                assert_eq!(admitted, None, "prefix {n} holds no whole frame");
            }
        }
    }

    #[test]
    fn a_header_that_lies_about_its_sizes_cannot_read_past_the_file() {
        // data size far beyond the file
        let mut huge = good_wav();
        put32(&mut huge, 40, 0xFFFF_FFF0);
        parse(&huge);
        // RIFF size far beyond the file
        let mut huge_riff = good_wav();
        put32(&mut huge_riff, 4, 0xFFFF_FFF0);
        assert!(parse(&huge_riff).is_some());
        // fmt chunk claiming a gigantic size (the walk must not seek off the
        // end)
        let mut huge_fmt = good_wav();
        put32(&mut huge_fmt, 16, 0xFFFF_FFF0);
        assert_eq!(parse(&huge_fmt), None);
        // zero-size chunks: a naive walk advances by 8 forever
        let mut zero_fmt = good_wav();
        put32(&mut zero_fmt, 16, 0);
        assert_eq!(parse(&zero_fmt), None);
    }

    #[test]
    fn impossible_format_fields_are_rejected() {
        let mut zero_channels = good_wav();
        put16(&mut zero_channels, 22, 0);
        let mut zero_rate = good_wav();
        put32(&mut zero_rate, 24, 0);
        let mut zero_bits = good_wav();
        put16(&mut zero_bits, 34, 0);
        // blockAlign inconsistent with channels * bits (the PCM-frame check)
        let mut bad_align = good_wav();
        put16(&mut bad_align, 32, 7);
        // a compressed codec tag
        let mut compressed = good_wav();
        put16(&mut compressed, 20, 0x0055); // MPEG layer 3
        for bytes in [zero_channels, zero_rate, zero_bits, bad_align, compressed] {
            assert_eq!(parse(&bytes), None);
        }
    }

    #[test]
    fn a_wall_of_chunks_does_not_make_the_walk_run_away() {
        // 4000 empty 'JUNK' chunks before 'data': the walk is bounded, so this
        // terminates (rejecting is fine) rather than scanning forever.
        let mut b = Riff::default();
        b.str("RIFF").u32(0).str("WAVE");
        for _ in 0..4000 {
            b.str("JUNK").u32(0);
        }
        assert_eq!(parse(&b.0), None);
    }

    #[test]
    fn a_directory_and_a_missing_path_are_rejected_not_thrown_at() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(admit_for_analysis(&dir.path().join("nope.wav")), None);
        assert_eq!(admit_for_analysis(dir.path()), None);
    }

    // --- manual memory measurements ------------------------------------------

    /// The process's resident set in bytes (Linux).
    #[cfg(target_os = "linux")]
    fn current_rss() -> u64 {
        let status = fs::read_to_string("/proc/self/status").unwrap();
        let line = status
            .lines()
            .find(|l| l.starts_with("VmRSS:"))
            .expect("VmRSS");
        let kb: u64 = line
            .split_whitespace()
            .nth(1)
            .and_then(|v| v.parse().ok())
            .expect("VmRSS kB");
        kb * 1024
    }

    /// Runs [work] while a sampler thread tracks the peak RSS; returns its
    /// result, the baseline and the peak.
    #[cfg(target_os = "linux")]
    fn peak_rss_of<T>(period: std::time::Duration, work: impl FnOnce() -> T) -> (T, u64, u64) {
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
        let baseline = current_rss();
        let peak = Arc::new(AtomicU64::new(baseline));
        let done = Arc::new(AtomicBool::new(false));
        let sampler = {
            let (peak, done) = (peak.clone(), done.clone());
            std::thread::spawn(move || {
                while !done.load(Ordering::SeqCst) {
                    peak.fetch_max(current_rss(), Ordering::SeqCst);
                    std::thread::sleep(period);
                }
            })
        };
        let out = work();
        done.store(true, Ordering::SeqCst);
        sampler.join().unwrap();
        (out, baseline, peak.load(Ordering::SeqCst))
    }

    /// Writes a mono 32-bit float WAV of [secs] seconds (the take format),
    /// one second at a time so the fixture itself never holds the track.
    #[cfg(target_os = "linux")]
    fn float_wav(path: &Path, sr: u32, secs: u32, sample: impl Fn(u32, u32) -> f32) {
        use std::io::Write;
        let frames = sr * secs;
        let mut file = io::BufWriter::new(File::create(path).unwrap());
        file.write_all(&wav(3, 1, sr, 32, &[])[..40]).unwrap();
        file.write_all(&(frames * 4).to_le_bytes()).unwrap();
        let mut block = Vec::with_capacity(sr as usize * 4);
        for s in 0..secs {
            block.clear();
            for i in 0..sr {
                block.extend_from_slice(&sample(s, i).to_le_bytes());
            }
            file.write_all(&block).unwrap();
        }
        file.flush().unwrap();
    }

    /// spectrogram_memory_test.dart. A measurement: run it alone, as
    /// `cargo test -p vocara_services --lib -- --ignored --test-threads=1
    /// a_long_wide_range`, so no other test's allocations land in the
    /// window.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "manual RSS measurement over a 115 MB fixture; run alone"]
    fn a_long_wide_range_spectrogram_peaks_near_its_own_size_not_twice_it() {
        // The 18 kHz EQ display spectrogram of a long take is the largest one
        // the app builds (769 bins a frame). Every column used to be its own
        // list until the end, which then copied them all into the flat image:
        // the compute held the whole result twice. Its frame count is known
        // from the header, so it now fills one buffer of exactly that size.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("take.wav");
        let (sr, secs) = (48000, 600);
        float_wav(&path, sr, secs, |s, i| {
            (0.4 * (2.0 * PI * 220.0 * f64::from(s * sr + i) / f64::from(sr)).sin()) as f32
        });
        let params = AnalysisParams {
            max_hz: 18000.0,
            ..AnalysisParams::default()
        };
        let (spec, baseline, peak) = peak_rss_of(std::time::Duration::from_millis(2), || {
            SpectrogramService.compute_from_file(&path.to_string_lossy(), params, None)
        });
        let spec = spec.unwrap().expect("a spectrogram");
        let result_bytes = (spec.db.len() * 4) as u64;
        let grew = peak - baseline;
        assert!(
            (grew as f64) < result_bytes as f64 * 1.75,
            "peak growth {} MB for a {} MB spectrogram",
            grew / 1_000_000,
            result_bytes / 1_000_000
        );
    }

    /// measure_analysis_peak_test.dart: MANUAL measurement, not a check. It
    /// generates the largest legal analysis WAV (10 min, 48 kHz, mono f32),
    /// runs compute_both_from_file, and prints the process RSS delta. Run it
    /// alone, like the test above.
    ///
    /// SCOPE: this measures the WAV path only — the windowed
    /// StreamingTakeAnalyzer, which never materializes the track. It is NOT
    /// the arbitrary-media path (MediaAnalysisService), which decodes through
    /// the native engine and holds a full-track buffer; a unit test cannot
    /// load the engine, so that path stays a derivation rather than a
    /// measurement. Quoting this number for it would understate it by the
    /// whole track.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "manual RSS measurement over a 115 MB fixture; prints, run alone"]
    fn measure_compute_both_from_file_peak_rss() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("max.wav");
        let sr = 48000;
        // A 220 Hz tone chunked out in 1 s blocks (silence would let clever
        // paths shortcut; a tone exercises the full DSP). Dart's fixture
        // wrapped the phase at a literal 6.28, not at 2π.
        #[allow(clippy::approx_constant)]
        let wrap = 6.28;
        float_wav(&path, sr, 600, |_, i| {
            (0.5 * ((f64::from(i * 220 * 2) * PI / f64::from(sr)) % wrap)) as f32
        });
        let (both, baseline, peak) = peak_rss_of(std::time::Duration::from_millis(50), || {
            SpectrogramService.compute_both_from_file(
                &path.to_string_lossy(),
                AnalysisParams::default(),
                None,
            )
        });
        let both = both.unwrap().expect("an analysis");
        assert!(both.spectrogram.is_some());
        assert!(both.metrics.is_some());
        println!(
            "compute_both_from_file 10min@48k f32: RSS delta ≈ {} MB (baseline {} MB, peak {} MB)",
            (peak - baseline) / 1_000_000,
            baseline / 1_000_000,
            peak / 1_000_000
        );
    }
}
